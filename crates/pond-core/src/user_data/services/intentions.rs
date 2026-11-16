//! The heartbeat's answer as intentions: the reviewer's recipe, how the jobs it leaves itself
//! are read and checked, the critic that drops the idle ones, and what fires one.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde_json::{Map, Value};

use super::extraction_gate::{moment_said, numbered_drops, section, REPLY_CUE};
use crate::models::services::thought_filter::strip_reasoning;
use crate::shared::domain::todo::{clock_label, place_sentence, HookSpec, Transition};
use crate::user_data::domain::anchor::{Anchor, AnchorState};
use crate::user_data::domain::intention::{
    Condition, IntentAct, Intention, IntentionRefused, IntentionState,
};
use crate::user_data::domain::interest::topic_key;
use crate::user_data::services::model_json::text_of;

/// Jobs one review may leave itself.
pub const MAX_INTENTS_PER_RUN: usize = 3;
/// Jobs one member may have waiting.
pub const MAX_WAITING: usize = 20;
/// Days off a job's deadline may fall, by the member's calendar.
pub const MAX_DEADLINE_DAYS: i64 = 30;
const MAX_WHY_CHARS: usize = 200;
const MAX_LINE_CHARS: usize = 240;
const CHAT_LABEL: &str = "When you next talk to Goose";

/// The shipped recipe: a `const`, not an `agent_recipes` row, so nobody can widen its
/// `tool_groups` at run time. The date comes via the brief: subagents lack `get_current_time`.
pub const HEARTBEAT_RECIPE: &str = r#"
version: 1.0.0
title: Proactive reviewer
description: Looks in on the household while it is quiet and leaves itself the jobs that will help.
giap_role:
  tool_groups:
    - giap-memory
    - giap-device
    - giap-sensors
    - giap-weather
  personal_data: inherit
  max_turns: 6
  context_fraction: 0.35
  instructions: |
    You are looking in on this household for the person you work for, while
    the house is quiet. Go through your checklist, what has happened and what
    they have coming up, and leave yourself the few jobs that will help them.
    Keep your thinking short. Be specific: connect actual events, readings,
    forecasts and memories to each job.

    You have no clock and no calendar. The current time is stated in the brief;
    do not guess at it and do not use any other time.

    Look things up with at most three tool calls, then answer. Your last
    message must be the answer.

    Answer with a JSON array and nothing else: no words before or after it. An
    empty array, [], is a complete and common answer. Every element starts
    with "do" and is one of these.

    A job you leave yourself, done once its condition holds:

      {"do": "intend",
       "act": "push" | "refresh" | "look_into" | "ask" | "propose_todo",
       "text": "what to do or say, in full",
       "when": {"time": "a day and an hour"},
       "until": "the deadline, as a day and an hour",
       "about": "words from the title of a plan of theirs, or omit",
       "why": "one sentence"}
        push: tell them "text" on their phone.
        refresh: update what you prepared for the plan in "about".
        look_into: find "text" out and tell them the answer.
        ask: put the open questions on the plan in "about" to them.
        propose_todo: offer "text" as a to-do they can add or decline.
        "when" is one condition from the brief's list. Besides a time:
          {"place": "a place label", "on": "arrive" | "leave"}
          {"thing": "a source label", "event": "one of its events"}
          {"chat": true}
        A job with no such condition, or no deadline, is not kept.
        At most three jobs, and none the brief says is already waiting.

    Something only they can decide, shown to them to approve:

      {"do": "suggest",
       "trigger_kind": "camera" | "sensor" | "device" | "time" | "presence",
       "source_id": "the device, camera or sensor it came from, or omit",
       "signal": "the reading or event type, or omit",
       "rationale": "why this matters to this person, in one sentence",
       "suggestion": "what you would do for them if they say yes, as one instruction to yourself",
       "confidence": 0.0 to 1.0}
        Only for what changes a device, spends, sends or schedules: "Turn the
        porch light off at midnight". Never a question, a greeting or a
        remark on what happened.

    You never set a reminder: offer one with "propose_todo". To find something
    out, leave a "look_into" job, never a "suggest". Never leave a job that
    only remarks on what happened. Someone being at home, arriving, leaving
    or moving about is ordinary: never raise it. When in doubt, leave it out.
    Never repeat a suggestion the brief says was already declined.
"#;

/// How many jobs a review may still leave a member who has `waiting` of them.
pub fn room_for(waiting: usize) -> usize {
    MAX_INTENTS_PER_RUN.min(MAX_WAITING.saturating_sub(waiting))
}

/// The condition a job names, as the reviewer wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhenDraft {
    Time {
        words: String,
    },
    Place {
        label: String,
        on: String,
    },
    Thing {
        label: String,
        event: String,
    },
    Chat,
    /// None of the four shapes, or more than one of them.
    Unreadable,
}

impl WhenDraft {
    fn read(value: Option<&Value>) -> Self {
        let Some(map) = value.and_then(Value::as_object) else {
            return Self::Unreadable;
        };
        let text = |key: &str| text_of(map, key);
        let chat = match map.get("chat") {
            Some(Value::Bool(yes)) => *yes,
            Some(Value::String(yes)) => yes.trim().eq_ignore_ascii_case("true"),
            _ => false,
        };
        let mut shapes: Vec<Self> = Vec::new();
        if let Some(words) = text("time") {
            shapes.push(Self::Time { words });
        }
        if let Some(label) = text("place") {
            shapes.push(Self::Place {
                label,
                on: text("on").unwrap_or_default(),
            });
        }
        if let Some(label) = text("thing") {
            shapes.push(Self::Thing {
                label,
                event: text("event").unwrap_or_default(),
            });
        }
        if chat {
            shapes.push(Self::Chat);
        }
        match shapes.len() {
            1 => shapes.remove(0),
            _ => Self::Unreadable,
        }
    }
}

/// One job the reviewer left itself, read and not yet checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentDraft {
    pub act: IntentAct,
    pub text: String,
    pub when: WhenDraft,
    /// The deadline in the reviewer's words; empty when it gave none.
    pub until: String,
    /// Words from the title of the anchor it is for.
    pub about: Option<String>,
    pub why: String,
}

/// The jobs one answer holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IntentParse {
    pub drafts: Vec<IntentDraft>,
    /// `intend` elements with no act the heartbeat has, or no words.
    pub unreadable: usize,
}

/// A word as a small model spells it: any case, with a space or a hyphen for the underscore.
fn spelled(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .split([' ', '-', '_'])
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn first_value(text: &str) -> Option<(Value, usize)> {
    let mut stream = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    let value = stream.next()?.ok()?;
    Some((value, stream.byte_offset()))
}

/// The elements an answer holds: its JSON array, or else every whole object that has a `do`,
/// as an answer cut short or written without its brackets leaves them.
fn elements_in(answer: &str) -> Vec<Value> {
    let cleaned = strip_reasoning(answer);
    for (at, _) in cleaned.match_indices('[') {
        // Prose brackets open on a word; an answer opens on an object or closes at once.
        if !cleaned[at + 1..].trim_start().starts_with(['{', ']']) {
            continue;
        }
        if let Some((Value::Array(items), _)) = first_value(&cleaned[at..]) {
            return items;
        }
    }
    let mut found: Vec<Value> = Vec::new();
    let mut from = 0;
    while let Some(at) = cleaned[from..].find('{').map(|offset| from + offset) {
        match first_value(&cleaned[at..]) {
            Some((value, length)) if value.get("do").is_some() => {
                found.push(value);
                from = at + length;
            }
            _ => from = at + 1,
        }
    }
    found
}

fn is_intent(map: &Map<String, Value>) -> bool {
    map.get("do")
        .and_then(Value::as_str)
        .is_some_and(|word| matches!(spelled(word).as_str(), "intend" | "intent" | "intention"))
}

fn draft_of(map: &Map<String, Value>) -> Option<IntentDraft> {
    let act = IntentAct::parse(&spelled(map.get("act")?.as_str()?))?;
    Some(IntentDraft {
        act,
        text: text_of(map, "text")?,
        when: WhenDraft::read(map.get("when")),
        until: text_of(map, "until").unwrap_or_default(),
        about: text_of(map, "about"),
        why: text_of(map, "why")
            .or_else(|| text_of(map, "rationale"))
            .map(|why| why.chars().take(MAX_WHY_CHARS).collect())
            .unwrap_or_default(),
    })
}

/// The jobs in the reviewer's answer, at most [`MAX_INTENTS_PER_RUN`]. Elements whose `do`
/// is anything else are left for the reader of suggestions.
pub fn parse_intents(answer: &str) -> IntentParse {
    let mut parse = IntentParse::default();
    for element in elements_in(answer) {
        let Some(map) = element.as_object().filter(|map| is_intent(map)) else {
            continue;
        };
        if parse.drafts.len() == MAX_INTENTS_PER_RUN {
            break;
        }
        match draft_of(map) {
            Some(draft) => parse.drafts.push(draft),
            None => parse.unreadable += 1,
        }
    }
    parse
}

/// A saved place of the member's a job may wait at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceChoice {
    pub id: String,
    pub label: String,
}

/// A camera, sensor or device from the hook catalogue and the events it raises, by their ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThingChoice {
    pub source_id: String,
    pub label: String,
    pub events: Vec<String>,
}

/// What a job's condition may name for one member.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConditionContext {
    pub places: Vec<PlaceChoice>,
    pub things: Vec<ThingChoice>,
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The conditions as the brief lists them: time and chat always, then the places and things
/// by the labels a job must use.
pub fn conditions_block(ctx: &ConditionContext) -> String {
    let mut lines = vec![
        "Conditions a job may wait for:".to_string(),
        "- time: any day and hour still to come".to_string(),
        "- chat: the next time they talk to you".to_string(),
    ];
    let places: Vec<String> = ctx
        .places
        .iter()
        .map(|place| one_line(&place.label))
        .filter(|label| !label.is_empty())
        .collect();
    if !places.is_empty() {
        lines.push("- place, on arrive or leave:".to_string());
        lines.extend(places.into_iter().map(|label| format!("    {label}")));
    }
    let things: Vec<String> = ctx
        .things
        .iter()
        .filter(|thing| !thing.events.is_empty() && !one_line(&thing.label).is_empty())
        .map(|thing| {
            format!(
                "    {}: {}",
                one_line(&thing.label),
                thing.events.join(", ")
            )
        })
        .collect();
    if !things.is_empty() {
        lines.push("- thing, with its events:".to_string());
        lines.extend(things);
    }
    lines.join("\n")
}

/// Why a job the reviewer left itself was not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentRefusal {
    NoText,
    /// A waiting job, an open to-do or a schedule has the same key.
    AlreadyThere,
    NoCondition,
    NoTime,
    UnknownPlace,
    UnknownCrossing,
    UnknownThing,
    UnknownEvent,
    NoDeadline,
    DeadlineTooFar,
    DeadlineBeforeCondition,
    /// A refresh or a question with no anchor to refresh or ask about.
    NoPlan,
}

impl IntentRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoText => "it has no words",
            Self::AlreadyThere => "a waiting job, an open to-do or a schedule already says it",
            Self::NoCondition => "its `when` is not one of the four conditions",
            Self::NoTime => "its time names no moment still to come",
            Self::UnknownPlace => "its place is not one saved place of the member's",
            Self::UnknownCrossing => "its `on` is neither arrive nor leave",
            Self::UnknownThing => "its thing is not one source this pond has",
            Self::UnknownEvent => "its event is not one the source raises",
            Self::NoDeadline => "its `until` names no time still to come",
            Self::DeadlineTooFar => "its deadline is more than 30 days off",
            Self::DeadlineBeforeCondition => "its deadline falls before its time",
            Self::NoPlan => "it names no one plan of the member's to refresh or ask about",
        }
    }
}

fn only<T>(mut found: impl Iterator<Item = T>) -> Option<T> {
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

fn same_label(listed: &str, said: &str) -> bool {
    one_line(listed).to_lowercase() == one_line(said).to_lowercase()
}

fn condition_of(
    when: &WhenDraft,
    ctx: &ConditionContext,
    now: DateTime<Utc>,
    tz: Tz,
) -> Result<(Condition, String), IntentRefusal> {
    match when {
        WhenDraft::Time { words } => {
            let at = moment_said(words, now, tz).ok_or(IntentRefusal::NoTime)?;
            let label = clock_label(&HookSpec::Time { at }, now, tz);
            Ok((Condition::Time { at }, label))
        }
        WhenDraft::Place { label, on } => {
            let place = only(ctx.places.iter().filter(|p| same_label(&p.label, label)))
                .ok_or(IntentRefusal::UnknownPlace)?;
            let transition = Transition::parse(on).ok_or(IntentRefusal::UnknownCrossing)?;
            Ok((
                Condition::Place {
                    place_id: place.id.clone(),
                    transition,
                },
                place_sentence(&place.label, transition),
            ))
        }
        WhenDraft::Thing { label, event } => {
            let thing = only(ctx.things.iter().filter(|t| same_label(&t.label, label)))
                .ok_or(IntentRefusal::UnknownThing)?;
            let event = thing
                .events
                .iter()
                .find(|listed| spelled(listed) == spelled(event))
                .ok_or(IntentRefusal::UnknownEvent)?;
            Ok((
                Condition::Thing {
                    source_id: thing.source_id.clone(),
                    event: event.clone(),
                },
                format!(
                    "When {} reports {}",
                    one_line(&thing.label),
                    event.replace('_', " ")
                ),
            ))
        }
        WhenDraft::Chat => Ok((Condition::Chat, CHAT_LABEL.to_string())),
        WhenDraft::Unreadable => Err(IntentRefusal::NoCondition),
    }
}

/// A deadline said without an hour runs to the end of its day.
fn deadline_of(words: &str, now: DateTime<Utc>, tz: Tz) -> Result<DateTime<Utc>, IntentRefusal> {
    // Read alone first, or words that name no day would become tonight. An hour in the words
    // is read after the one put before them and replaces it.
    let until = moment_said(words, now, tz)
        .and_then(|_| moment_said(&format!("at 23:59 {words}"), now, tz))
        .ok_or(IntentRefusal::NoDeadline)?;
    let today = now.with_timezone(&tz).date_naive();
    if (until.with_timezone(&tz).date_naive() - today).num_days() > MAX_DEADLINE_DAYS {
        return Err(IntentRefusal::DeadlineTooFar);
    }
    Ok(until)
}

/// The member's active anchor whose title has every word of `words`, when exactly one does.
fn anchor_named<'a>(anchors: &'a [Anchor], words: &str, profile_id: &str) -> Option<&'a Anchor> {
    let key = topic_key(words);
    if key.is_empty() {
        return None;
    }
    only(anchors.iter().filter(|anchor| {
        let title = anchor.key();
        anchor.state == AnchorState::Active
            && anchor.is_readable_by(profile_id)
            && key
                .split(' ')
                .all(|word| title.split(' ').any(|held| held == word))
    }))
}

/// A draft as a waiting intention, or why it is not kept. `taken_keys` holds the keys of the
/// member's waiting intentions, open to-dos and schedules; add each kept job's key before the
/// next draft is checked.
pub fn validate(
    draft: &IntentDraft,
    ctx: &ConditionContext,
    taken_keys: &HashSet<String>,
    anchors: &[Anchor],
    profile_id: &str,
    now: DateTime<Utc>,
    tz: Tz,
) -> Result<Intention, IntentRefusal> {
    if topic_key(&draft.text).is_empty() {
        return Err(IntentRefusal::NoText);
    }
    let (condition, label) = condition_of(&draft.when, ctx, now, tz)?;
    let until = deadline_of(&draft.until, now, tz)?;
    let anchor = draft
        .about
        .as_deref()
        .and_then(|words| anchor_named(anchors, words, profile_id));
    if anchor.is_none() && matches!(draft.act, IntentAct::Refresh | IntentAct::Ask) {
        return Err(IntentRefusal::NoPlan);
    }
    let mut intention = Intention::new(
        profile_id,
        draft.act,
        &draft.text,
        condition,
        &label,
        until,
        now,
    )
    .map_err(|refused| match refused {
        IntentionRefused::NoText => IntentRefusal::NoText,
        IntentionRefused::DeadlinePassed => IntentRefusal::NoDeadline,
        IntentionRefused::ConditionAfterDeadline => IntentRefusal::DeadlineBeforeCondition,
    })?;
    // The stored text is bounded, so the key is the stored one's.
    if taken_keys.contains(&intention.key()) {
        return Err(IntentRefusal::AlreadyThere);
    }
    intention.why = draft.why.clone();
    intention.anchor_id = anchor.map(|anchor| anchor.id.clone());
    Ok(intention)
}

/// Fixed so it stays in the KV prefix. It shows no worked ruling: a small model copies one.
pub const INTENT_CRITIC_SYSTEM: &str = "\
You check the jobs Goose has left itself to do later for a member of this \
household, before any of them is kept. You are the last check, so look for \
the jobs that would not help.

You are given what has happened in the house, the member's plans, and the \
numbered jobs, each with what it waits for and why it was left.

Drop a job when any of these is true:
- it only remarks on what happened, and does nothing for the member;
- it is about someone being at home, arriving, leaving or moving about. What \
a job waits for does not count: one that waits for the member to arrive \
somewhere is fine;
- it repeats another job in the list;
- it serves no plan of theirs, no event that happened and nothing they said \
they want.

Keep every other job. Wording you would have chosen differently is not a \
reason to drop one.

Reply with JSON and nothing else:

{\"drop\": [{\"n\": <the job's number>, \"why\": \"<a few words>\"}]}

- Use only the numbers you were given. An empty list when every job is worth keeping.
- \"why\": one line, under 100 characters.
- No explanation, no preamble.";

const HAPPENED_HEADING: &str = "What has happened:";
const PLANS_HEADING: &str = "The member's plans:";
const JOBS_HEADING: &str = "Jobs to check:";

fn act_in_words(act: IntentAct) -> &'static str {
    match act {
        IntentAct::Push => "Tell them",
        IntentAct::Refresh => "Bring up to date",
        IntentAct::LookInto => "Find out and tell them",
        IntentAct::Ask => "Ask them the open questions",
        IntentAct::ProposeTodo => "Offer them a to-do",
    }
}

/// The critic's user message: what happened, the member's anchors as already rendered, then
/// the jobs numbered from 1.
pub fn build_intent_critic_message(
    intentions: &[Intention],
    happened: &str,
    anchors: &str,
) -> String {
    let jobs: Vec<String> = intentions
        .iter()
        .enumerate()
        .map(|(at, job)| {
            let mut lines = format!(
                "{}. {}: {}\n   Waits for: {}",
                at + 1,
                act_in_words(job.act),
                job.text,
                job.condition_label
            );
            if !job.why.is_empty() {
                lines.push_str(&format!("\n   Why: {}", job.why));
            }
            lines
        })
        .collect();
    let mut out = String::new();
    section(&mut out, HAPPENED_HEADING, happened);
    section(&mut out, PLANS_HEADING, anchors);
    section(&mut out, JOBS_HEADING, &jobs.join("\n"));
    out.push_str(REPLY_CUE);
    out
}

/// Which of `count` jobs the critic dropped, as places from 0 with its reason; `None` when the
/// reply cannot be read, and then no job is kept.
pub fn parse_intent_drops(raw: &str, count: usize) -> Option<Vec<(usize, String)>> {
    numbered_drops(raw, count)
}

/// The brief's list of the jobs still waiting, each with what it waits for and its deadline;
/// empty when there are none.
pub fn waiting_block(waiting: &[Intention], tz: Tz) -> String {
    let lines: Vec<String> = waiting
        .iter()
        .filter(|job| job.state == IntentionState::Waiting)
        .map(|job| {
            format!(
                "- {}: {} [{}; until {}]",
                job.act.as_str(),
                job.text,
                job.condition_label,
                job.until.with_timezone(&tz).format("%-d %B at %H:%M")
            )
        })
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    format!("Jobs you left yourself:\n{}", lines.join("\n"))
}

/// Something that happened which a waiting job may have been waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionEvent {
    /// The member's phone reported crossing one of their saved places.
    Place {
        place_id: String,
        transition: Transition,
    },
    /// A source from the hook catalogue raised an event.
    Thing { source_id: String, event: String },
    /// A member's turn began.
    Chat { profile_id: String },
}

/// Whether a waiting intention's condition is met by `event`; a time condition never is.
pub fn fires_on(intention: &Intention, event: &ConditionEvent) -> bool {
    if intention.state != IntentionState::Waiting {
        return false;
    }
    match event {
        ConditionEvent::Place {
            place_id,
            transition,
        } => intention
            .condition
            .is_met_by_crossing(place_id, *transition),
        ConditionEvent::Thing { source_id, event } => {
            intention.condition.is_met_by_event(source_id, event)
        }
        ConditionEvent::Chat { profile_id } => {
            intention.condition == Condition::Chat && intention.profile_id == *profile_id
        }
    }
}

/// One line for the member's activity log about an intention that was settled.
pub fn settled_line(intention: &Intention, outcome: &str) -> String {
    let lead = match (intention.state, intention.act) {
        (IntentionState::Expired, _) => "Let go, its time ran out",
        (IntentionState::Dropped, _) => "Withdrawn",
        (_, IntentAct::Push) => "Told you",
        (_, IntentAct::Refresh) => "Brought up to date",
        (_, IntentAct::LookInto) => "Looked into",
        (_, IntentAct::Ask) => "Asked you",
        (_, IntentAct::ProposeTodo) => "Offered you a to-do",
    };
    let mut line = format!("{lead}: {}", intention.text);
    let outcome = one_line(outcome);
    if !outcome.is_empty() {
        if !line.ends_with(['.', '!', '?']) {
            line.push('.');
        }
        line.push(' ');
        line.push_str(&outcome);
    }
    line.chars().take(MAX_LINE_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::orchestration::AgentRole;
    use crate::user_data::domain::anchor::{AnchorKind, AnchorOrigin};
    use crate::user_data::services::proactive_review::{
        PROACTIVE_REVIEWER_ROLE, PROACTIVE_ROOT_GROUPS,
    };
    use chrono::{Duration, TimeZone};

    const NAIROBI: Tz = chrono_tz::Africa::Nairobi;

    /// Monday 16 November 2026, 12:00 in Nairobi.
    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn nairobi(month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        NAIROBI
            .with_ymd_and_hms(2026, month, day, hour, minute, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn ctx() -> ConditionContext {
        ConditionContext {
            places: vec![
                PlaceChoice {
                    id: "place-1".into(),
                    label: "Office".into(),
                },
                PlaceChoice {
                    id: "place-2".into(),
                    label: "Home".into(),
                },
            ],
            things: vec![
                ThingChoice {
                    source_id: "camera:gate".into(),
                    label: "Gate camera".into(),
                    events: vec!["person".into(), "motion".into()],
                },
                ThingChoice {
                    source_id: "cart:fielder".into(),
                    label: "Fielder on Kilimall".into(),
                    events: vec!["price_drop".into(), "back_in_stock".into()],
                },
            ],
        }
    }

    fn summit() -> Anchor {
        Anchor::new(
            "jerry",
            AnchorKind::Event,
            "AI Summit Nairobi",
            AnchorOrigin::You,
            now(),
        )
        .unwrap()
    }

    fn draft(when: WhenDraft) -> IntentDraft {
        IntentDraft {
            act: IntentAct::Push,
            text: "Leave by 7:15, the summit doors open at 8".into(),
            when,
            until: "3 December at 9am".into(),
            about: Some("AI Summit".into()),
            why: "The summit is on their calendar".into(),
        }
    }

    fn at_eight() -> WhenDraft {
        WhenDraft::Time {
            words: "3 December at 7am".into(),
        }
    }

    fn check(draft: &IntentDraft) -> Result<Intention, IntentRefusal> {
        validate(
            draft,
            &ctx(),
            &HashSet::new(),
            &[summit()],
            "jerry",
            now(),
            NAIROBI,
        )
    }

    fn refusal(draft: &IntentDraft) -> IntentRefusal {
        check(draft).expect_err("the draft should be refused")
    }

    fn kept(when: WhenDraft) -> Intention {
        check(&draft(when)).expect("the draft should be kept")
    }

    #[test]
    fn the_recipe_parses_as_the_reviewer_with_reading_groups_only() {
        let role = AgentRole::from_recipe_yaml(PROACTIVE_REVIEWER_ROLE, HEARTBEAT_RECIPE)
            .expect("the recipe is valid")
            .expect("the recipe has a role block");
        assert_eq!(role.name(), PROACTIVE_REVIEWER_ROLE);
        assert_eq!(role.max_turns(), 6);
        let groups: Vec<&str> = role
            .requested_tool_groups()
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(
            groups,
            ["giap-device", "giap-memory", "giap-sensors", "giap-weather"]
        );
        assert!(groups.iter().all(|g| PROACTIVE_ROOT_GROUPS.contains(g)));
        assert!(role.requested_model().is_none(), "no model is named");
    }

    #[test]
    fn the_recipe_shows_a_job_with_every_act_and_condition_and_no_reminder_or_note() {
        let role = AgentRole::from_recipe_yaml(PROACTIVE_REVIEWER_ROLE, HEARTBEAT_RECIPE)
            .unwrap()
            .unwrap();
        let said = role.instructions();
        for element in ["intend", "suggest"] {
            assert!(
                said.contains(&format!("{{\"do\": \"{element}\"")),
                "{element}"
            );
        }
        for removed in ["remind", "note", "done", "look_into", "pause_watch"] {
            assert!(
                !said.contains(&format!("{{\"do\": \"{removed}\"")),
                "{removed}"
            );
        }
        for act in [
            IntentAct::Push,
            IntentAct::Refresh,
            IntentAct::LookInto,
            IntentAct::Ask,
            IntentAct::ProposeTodo,
        ] {
            assert!(
                said.contains(&format!("\n    {}: ", act.as_str())),
                "{act:?}"
            );
        }
        for shape in [
            "{\"time\": ",
            "{\"place\": ",
            "{\"thing\": ",
            "{\"chat\": true}",
        ] {
            assert!(said.contains(shape), "{shape}");
        }
        assert!(said.contains("at most three tool calls"));
        assert!(said.contains("At most three jobs"));
        assert_eq!(MAX_INTENTS_PER_RUN, 3);
        assert!(said.contains("An\nempty array, [], is a complete and common answer"));
        let (intending, asking) = (
            said.find("\"do\": \"intend\"").unwrap(),
            said.find("\"do\": \"suggest\"").unwrap(),
        );
        assert!(intending < asking, "doing is shown before asking");
        for never in [
            "You never set a reminder",
            "only remarks on what happened",
            "Someone being at home",
        ] {
            assert!(said.contains(never), "{never}");
        }
        assert!(HEARTBEAT_RECIPE.chars().count() <= 3_300);
    }

    #[test]
    fn the_shapes_the_recipe_shows_are_refused_when_copied_as_they_stand() {
        let copied = r#"[{"do": "intend", "act": "push", "text": "what to do or say, in full",
            "when": {"time": "a day and an hour"}, "until": "the deadline, as a day and an hour",
            "about": "words from the title of a plan of theirs, or omit", "why": "one sentence"}]"#;
        let read = parse_intents(copied);
        assert_eq!(read.drafts.len(), 1);
        assert_eq!(refusal(&read.drafts[0]), IntentRefusal::NoTime);
        let mut timed = read.drafts[0].clone();
        timed.when = at_eight();
        assert_eq!(refusal(&timed), IntentRefusal::NoDeadline);
    }

    const ANSWER: &str = r#"[
        {"do": "intend", "act": "push", "text": "Leave by 7:15, the summit doors open at 8",
         "when": {"time": "3 December at 7am"}, "until": "3 December at 9am",
         "about": "AI Summit", "why": "The summit is on their calendar"},
        {"do": "suggest", "trigger_kind": "time", "rationale": "It is late",
         "suggestion": "Turn the porch light off at midnight", "confidence": 0.8},
        {"do": "intend", "act": "look_into", "text": "Whether the Kisumu flight still leaves at 6",
         "when": {"chat": true}, "until": "Friday", "why": "They fly on Saturday"},
        {"do": "pause_watch", "text": "Fielder", "confidence": 0.9}
    ]"#;

    #[test]
    fn jobs_read_from_the_reviewers_array_and_every_other_element_is_left_alone() {
        let read = parse_intents(ANSWER);
        assert_eq!(read.unreadable, 0);
        assert_eq!(read.drafts.len(), 2);
        assert_eq!(read.drafts[0], draft(at_eight()));
        let second = &read.drafts[1];
        assert_eq!(second.act, IntentAct::LookInto);
        assert_eq!(second.when, WhenDraft::Chat);
        assert_eq!(second.until, "Friday");
        assert_eq!(second.about, None);
        assert_eq!(second.why, "They fly on Saturday");

        assert_eq!(parse_intents("[]"), IntentParse::default());
        assert_eq!(parse_intents("Nothing to do."), IntentParse::default());
        assert_eq!(
            parse_intents(r#"[{"do": "suggest", "suggestion": "x"}]"#),
            IntentParse::default()
        );
    }

    #[test]
    fn each_shape_of_when_reads_and_anything_else_is_unreadable() {
        let when = |shape: &str| {
            let answer = format!(
                r#"[{{"do": "intend", "act": "push", "text": "Say it", "when": {shape}}}]"#
            );
            parse_intents(&answer).drafts.remove(0).when
        };
        assert_eq!(
            when(r#"{"time": "tomorrow at  8am"}"#),
            WhenDraft::Time {
                words: "tomorrow at 8am".into()
            }
        );
        assert_eq!(
            when(r#"{"place": "office", "on": "arrive"}"#),
            WhenDraft::Place {
                label: "office".into(),
                on: "arrive".into()
            }
        );
        assert_eq!(
            when(r#"{"thing": "gate camera", "event": "person"}"#),
            WhenDraft::Thing {
                label: "gate camera".into(),
                event: "person".into()
            }
        );
        assert_eq!(when(r#"{"chat": true}"#), WhenDraft::Chat);
        assert_eq!(when(r#"{"chat": "true"}"#), WhenDraft::Chat);
        assert_eq!(
            when(r#"{"time": "8am", "chat": false}"#),
            WhenDraft::Time {
                words: "8am".into()
            }
        );
        for unreadable in [
            r#""tomorrow at 8am""#,
            r#"{"chat": false}"#,
            r#"{"time": "8am", "chat": true}"#,
            r#"{"place": "office", "thing": "gate camera"}"#,
            r#"{"time": ""}"#,
            r#"{"when": "later"}"#,
            "null",
            "[]",
        ] {
            assert_eq!(when(unreadable), WhenDraft::Unreadable, "{unreadable}");
        }
    }

    #[test]
    fn a_job_with_no_act_the_heartbeat_has_or_no_words_is_counted_and_not_kept() {
        let read = parse_intents(
            r#"[{"do": "intend", "act": "remind", "text": "Call the dentist", "when": {"chat": true}},
                {"do": "intend", "text": "No act", "when": {"chat": true}},
                {"do": "intend", "act": "push", "text": " ", "when": {"chat": true}},
                {"do": "intend", "act": "push", "text": "...", "when": {"chat": true}},
                {"do": "Intend", "act": "Propose todo", "text": "Renew the car insurance",
                 "when": {"chat": true}, "rationale": "It lapses this month"}]"#,
        );
        assert_eq!(read.unreadable, 4);
        assert_eq!(read.drafts.len(), 1);
        assert_eq!(read.drafts[0].act, IntentAct::ProposeTodo);
        assert_eq!(read.drafts[0].why, "It lapses this month");
        assert_eq!(read.drafts[0].until, "");
    }

    #[test]
    fn the_shapes_a_small_model_writes_all_read() {
        let one = r#"{"do": "intend", "act": "ask", "text": "Ask about the trip",
                      "when": {"chat": true}, "until": "Friday", "about": "Kisumu"}"#;
        for answer in [
            format!("[{one}]"),
            format!("```json\n[{one}]\n```"),
            format!("Here is what I will do [see below]:\n[{one}]\nThat is all."),
            format!("<think>what helps [them]?</think>[{one}]"),
            one.to_string(),
            format!("[{one}, {{\"do\": \"intend\", \"act\": \"push\", \"text\": \"cut sho"),
        ] {
            let read = parse_intents(&answer);
            assert_eq!(read.drafts.len(), 1, "{answer}");
            assert_eq!(read.drafts[0].act, IntentAct::Ask, "{answer}");
            assert_eq!(read.drafts[0].when, WhenDraft::Chat, "{answer}");
        }
    }

    #[test]
    fn a_run_keeps_three_jobs_at_most_and_fewer_when_many_are_waiting() {
        let jobs: Vec<String> = (1..=5)
            .map(|n| {
                format!(
                    r#"{{"do": "intend", "act": "push", "text": "Job number {n}", "when": {{"chat": true}}}}"#
                )
            })
            .collect();
        let read = parse_intents(&format!("[{}]", jobs.join(",")));
        assert_eq!(read.drafts.len(), MAX_INTENTS_PER_RUN);
        assert_eq!(read.drafts[2].text, "Job number 3");
        assert_eq!(room_for(0), 3);
        assert_eq!(room_for(MAX_WAITING - 1), 1);
        assert_eq!(room_for(MAX_WAITING), 0);
        assert_eq!(room_for(MAX_WAITING + 5), 0);
    }

    #[test]
    fn a_time_job_is_kept_with_its_time_deadline_anchor_and_reason() {
        let anchors = [summit()];
        let job = validate(
            &draft(at_eight()),
            &ctx(),
            &HashSet::new(),
            &anchors,
            "jerry",
            now(),
            NAIROBI,
        )
        .unwrap();
        assert_eq!(
            job.condition,
            Condition::Time {
                at: nairobi(12, 3, 7, 0)
            }
        );
        assert_eq!(job.condition_label, "On Thursday 3 December at 07:00");
        assert_eq!(job.until, nairobi(12, 3, 9, 0));
        assert_eq!(job.act, IntentAct::Push);
        assert_eq!(job.text, "Leave by 7:15, the summit doors open at 8");
        assert_eq!(job.why, "The summit is on their calendar");
        assert_eq!(job.anchor_id.as_deref(), Some(anchors[0].id.as_str()));
        assert_eq!(job.profile_id, "jerry");
        assert_eq!(job.state, IntentionState::Waiting);
        assert_eq!(job.created_at, now());
    }

    #[test]
    fn a_place_job_names_one_saved_place_by_its_label_whatever_the_case() {
        let job = kept(WhenDraft::Place {
            label: " OFFICE ".into(),
            on: "arrive".into(),
        });
        assert_eq!(
            job.condition,
            Condition::Place {
                place_id: "place-1".into(),
                transition: Transition::Arrive
            }
        );
        assert_eq!(job.condition_label, "When you get to the office");
        let leaving = kept(WhenDraft::Place {
            label: "home".into(),
            on: "Leave".into(),
        });
        assert_eq!(leaving.condition_label, "When you leave home");

        let place = |label: &str, on: &str| {
            refusal(&draft(WhenDraft::Place {
                label: label.into(),
                on: on.into(),
            }))
        };
        assert_eq!(place("the gym", "arrive"), IntentRefusal::UnknownPlace);
        assert_eq!(place("Off", "arrive"), IntentRefusal::UnknownPlace);
        assert_eq!(place("office", "nearby"), IntentRefusal::UnknownCrossing);
        assert_eq!(place("office", ""), IntentRefusal::UnknownCrossing);
    }

    #[test]
    fn a_label_two_places_share_names_neither() {
        let mut twice = ctx();
        twice.places.push(PlaceChoice {
            id: "place-3".into(),
            label: "office".into(),
        });
        let refused = validate(
            &draft(WhenDraft::Place {
                label: "Office".into(),
                on: "arrive".into(),
            }),
            &twice,
            &HashSet::new(),
            &[summit()],
            "jerry",
            now(),
            NAIROBI,
        );
        assert_eq!(refused.unwrap_err(), IntentRefusal::UnknownPlace);
    }

    #[test]
    fn a_thing_job_names_a_source_by_its_label_and_an_event_it_raises() {
        let job = kept(WhenDraft::Thing {
            label: "gate camera".into(),
            event: "Person".into(),
        });
        assert_eq!(
            job.condition,
            Condition::Thing {
                source_id: "camera:gate".into(),
                event: "person".into()
            }
        );
        assert_eq!(job.condition_label, "When Gate camera reports person");
        let drop = kept(WhenDraft::Thing {
            label: "Fielder on Kilimall".into(),
            event: "price drop".into(),
        });
        assert_eq!(
            drop.condition,
            Condition::Thing {
                source_id: "cart:fielder".into(),
                event: "price_drop".into()
            }
        );
        assert_eq!(
            drop.condition_label,
            "When Fielder on Kilimall reports price drop"
        );

        let thing = |label: &str, event: &str| {
            refusal(&draft(WhenDraft::Thing {
                label: label.into(),
                event: event.into(),
            }))
        };
        assert_eq!(thing("porch camera", "person"), IntentRefusal::UnknownThing);
        assert_eq!(thing("camera:gate", "person"), IntentRefusal::UnknownThing);
        assert_eq!(thing("gate camera", "package"), IntentRefusal::UnknownEvent);
        assert_eq!(thing("gate camera", ""), IntentRefusal::UnknownEvent);
    }

    #[test]
    fn a_chat_job_waits_for_the_members_next_turn() {
        let job = kept(WhenDraft::Chat);
        assert_eq!(job.condition, Condition::Chat);
        assert_eq!(job.condition_label, "When you next talk to Goose");
    }

    #[test]
    fn a_job_with_no_words_or_no_checkable_condition_is_refused() {
        let mut wordless = draft(WhenDraft::Chat);
        wordless.text = " . ".into();
        assert_eq!(refusal(&wordless), IntentRefusal::NoText);
        assert_eq!(
            refusal(&draft(WhenDraft::Unreadable)),
            IntentRefusal::NoCondition
        );
        for words in [
            "when it rains",
            "soon",
            "today at 8am",
            "3 December 2027 at 8am",
        ] {
            let timed = draft(WhenDraft::Time {
                words: words.into(),
            });
            assert_eq!(refusal(&timed), IntentRefusal::NoTime, "{words}");
        }
    }

    #[test]
    fn a_job_already_waiting_or_on_a_list_or_a_schedule_is_refused() {
        let taken: HashSet<String> = [topic_key("leave by 7:15 -- the Summit doors open at 8!")]
            .into_iter()
            .collect();
        let refused = validate(
            &draft(WhenDraft::Chat),
            &ctx(),
            &taken,
            &[summit()],
            "jerry",
            now(),
            NAIROBI,
        );
        assert_eq!(refused.unwrap_err(), IntentRefusal::AlreadyThere);
        let job = kept(WhenDraft::Chat);
        assert!(taken.contains(&job.key()), "a kept job's key is the same");

        let mut long = draft(WhenDraft::Chat);
        long.text = "word ".repeat(100);
        let first = check(&long).unwrap();
        let taken: HashSet<String> = [first.key()].into_iter().collect();
        let again = validate(&long, &ctx(), &taken, &[summit()], "jerry", now(), NAIROBI);
        assert_eq!(again.unwrap_err(), IntentRefusal::AlreadyThere);
    }

    #[test]
    fn a_job_needs_a_deadline_still_to_come_and_no_more_than_thirty_days_off() {
        let until = |words: &str| {
            let mut job = draft(WhenDraft::Chat);
            job.until = words.into();
            check(&job)
        };
        for words in [
            "",
            "whenever",
            "the end of the week",
            "today at 8am",
            "before Friday",
        ] {
            assert_eq!(
                until(words).unwrap_err(),
                IntentRefusal::NoDeadline,
                "{words}"
            );
        }
        assert_eq!(
            until("17 December").unwrap_err(),
            IntentRefusal::DeadlineTooFar
        );
        assert_eq!(
            until("3 November").unwrap_err(),
            IntentRefusal::DeadlineTooFar
        );
        assert_eq!(until("16 December").unwrap().until, nairobi(12, 16, 23, 59));
        assert_eq!(
            until("in 2 hours").unwrap().until,
            now() + Duration::hours(2)
        );
    }

    #[test]
    fn a_deadline_said_without_an_hour_runs_to_the_end_of_its_day() {
        let until = |words: &str| {
            let mut job = draft(at_eight());
            job.until = words.into();
            check(&job).map(|job| job.until)
        };
        assert_eq!(until("3 December"), Ok(nairobi(12, 3, 23, 59)));
        assert_eq!(until("3 December at 9am"), Ok(nairobi(12, 3, 9, 0)));
        assert_eq!(
            until("Friday 4 December at 18:30"),
            Ok(nairobi(12, 4, 18, 30))
        );
        assert_eq!(until("today"), Err(IntentRefusal::DeadlineBeforeCondition));
        let today = {
            let mut job = draft(WhenDraft::Chat);
            job.until = "today".into();
            check(&job).map(|job| job.until)
        };
        assert_eq!(today, Ok(nairobi(11, 16, 23, 59)));
    }

    #[test]
    fn a_deadline_before_the_time_it_waits_for_is_refused() {
        let mut job = draft(at_eight());
        job.until = "2 December at 6pm".into();
        assert_eq!(refusal(&job), IntentRefusal::DeadlineBeforeCondition);
        job.until = "3 December at 6am".into();
        assert_eq!(refusal(&job), IntentRefusal::DeadlineBeforeCondition);
    }

    #[test]
    fn about_names_an_anchor_only_when_one_active_title_has_all_its_words() {
        let mut ended = Anchor::new(
            "jerry",
            AnchorKind::Trip,
            "Kisumu in December",
            AnchorOrigin::You,
            now(),
        )
        .unwrap();
        ended.stop(now()).unwrap();
        let candidate = Anchor::new(
            "jerry",
            AnchorKind::Trip,
            "Diani weekend",
            AnchorOrigin::Chat,
            now(),
        )
        .unwrap();
        let theirs = Anchor::new(
            "amina",
            AnchorKind::Event,
            "Wedding in Naivasha",
            AnchorOrigin::You,
            now(),
        )
        .unwrap();
        let other_summit = Anchor::new(
            "jerry",
            AnchorKind::Event,
            "Summit committee dinner",
            AnchorOrigin::You,
            now(),
        )
        .unwrap();
        let anchors = [summit(), ended, candidate, theirs, other_summit];
        let named = |about: Option<&str>, act: IntentAct| {
            let mut job = draft(WhenDraft::Chat);
            job.about = about.map(str::to_string);
            job.act = act;
            validate(
                &job,
                &ctx(),
                &HashSet::new(),
                &anchors,
                "jerry",
                now(),
                NAIROBI,
            )
            .map(|job| job.anchor_id)
        };
        let summit_id = Some(anchors[0].id.clone());
        assert_eq!(
            named(Some("ai summit"), IntentAct::Push),
            Ok(summit_id.clone())
        );
        assert_eq!(
            named(Some("Nairobi, AI!"), IntentAct::Refresh),
            Ok(summit_id)
        );
        for unnamed in [
            None,
            Some("summit"),
            Some("omit"),
            Some("Kisumu"),
            Some("Diani"),
            Some("Naivasha"),
            Some("the moon"),
            Some(" "),
        ] {
            assert_eq!(named(unnamed, IntentAct::Push), Ok(None), "{unnamed:?}");
            assert_eq!(named(unnamed, IntentAct::LookInto), Ok(None), "{unnamed:?}");
            assert_eq!(
                named(unnamed, IntentAct::ProposeTodo),
                Ok(None),
                "{unnamed:?}"
            );
            assert_eq!(
                named(unnamed, IntentAct::Refresh),
                Err(IntentRefusal::NoPlan),
                "{unnamed:?}"
            );
            assert_eq!(
                named(unnamed, IntentAct::Ask),
                Err(IntentRefusal::NoPlan),
                "{unnamed:?}"
            );
        }
    }

    #[test]
    fn every_refusal_says_why_in_plain_words() {
        let all = [
            IntentRefusal::NoText,
            IntentRefusal::AlreadyThere,
            IntentRefusal::NoCondition,
            IntentRefusal::NoTime,
            IntentRefusal::UnknownPlace,
            IntentRefusal::UnknownCrossing,
            IntentRefusal::UnknownThing,
            IntentRefusal::UnknownEvent,
            IntentRefusal::NoDeadline,
            IntentRefusal::DeadlineTooFar,
            IntentRefusal::DeadlineBeforeCondition,
            IntentRefusal::NoPlan,
        ];
        let said: HashSet<&str> = all.iter().map(|refusal| refusal.as_str()).collect();
        assert_eq!(said.len(), all.len());
        assert!(said
            .iter()
            .all(|why| why.starts_with("it") || why.starts_with("a ")));
        assert!(IntentRefusal::DeadlineTooFar
            .as_str()
            .contains(&MAX_DEADLINE_DAYS.to_string()));
    }

    #[test]
    fn the_brief_lists_the_conditions_by_the_labels_a_job_must_use() {
        assert_eq!(
            conditions_block(&ctx()),
            "Conditions a job may wait for:\n\
             - time: any day and hour still to come\n\
             - chat: the next time they talk to you\n\
             - place, on arrive or leave:\n    Office\n    Home\n\
             - thing, with its events:\n    Gate camera: person, motion\n    \
             Fielder on Kilimall: price_drop, back_in_stock"
        );
        let mut bare = ConditionContext::default();
        assert_eq!(
            conditions_block(&bare),
            "Conditions a job may wait for:\n\
             - time: any day and hour still to come\n\
             - chat: the next time they talk to you"
        );
        bare.things.push(ThingChoice {
            source_id: "device:lamp".into(),
            label: "Lamp".into(),
            events: Vec::new(),
        });
        assert!(
            !conditions_block(&bare).contains("Lamp"),
            "nothing to wait for"
        );
    }

    #[test]
    fn the_critic_is_shown_what_happened_the_plans_and_the_jobs_by_number() {
        let mut second = kept(WhenDraft::Place {
            label: "office".into(),
            on: "arrive".into(),
        });
        second.why.clear();
        second.act = IntentAct::ProposeTodo;
        let message = build_intent_critic_message(
            &[kept(at_eight()), second],
            "- The gate camera saw a person at 08:10\n",
            "- AI Summit Nairobi (event), 3 December",
        );
        assert_eq!(
            message,
            "What has happened:\n- The gate camera saw a person at 08:10\n\n\
             The member's plans:\n- AI Summit Nairobi (event), 3 December\n\n\
             Jobs to check:\n\
             1. Tell them: Leave by 7:15, the summit doors open at 8\n   \
             Waits for: On Thursday 3 December at 07:00\n   \
             Why: The summit is on their calendar\n\
             2. Offer them a to-do: Leave by 7:15, the summit doors open at 8\n   \
             Waits for: When you get to the office\n\n\
             JSON:"
        );
        let bare = build_intent_critic_message(&[kept(WhenDraft::Chat)], "", " ");
        assert!(bare.starts_with("Jobs to check:\n1. Tell them: "), "{bare}");
    }

    #[test]
    fn the_critics_drops_read_with_their_reasons_and_an_unreadable_reply_is_none() {
        assert_eq!(
            parse_intent_drops(
                r#"{"drop": [{"n": 2, "why": "only remarks on the gate camera"}]}"#,
                2
            ),
            Some(vec![(1, "only remarks on the gate camera".to_string())])
        );
        assert_eq!(parse_intent_drops(r#"{"drop": []}"#, 2), Some(vec![]));
        for raw in [
            "",
            "Both jobs look useful.",
            r#"{"drop": [3]}"#,
            INTENT_CRITIC_SYSTEM,
        ] {
            assert_eq!(parse_intent_drops(raw, 2), None, "{raw}");
        }
    }

    #[test]
    fn the_critic_prompt_names_every_reason_to_drop_a_job() {
        for reason in [
            "only remarks on what happened",
            "someone being at home",
            "repeats another job",
            "serves no plan of theirs, no event that happened",
        ] {
            assert!(INTENT_CRITIC_SYSTEM.contains(reason), "{reason}");
        }
    }

    #[test]
    fn the_brief_lists_the_waiting_jobs_with_what_they_wait_for_and_their_deadline() {
        let mut done = kept(WhenDraft::Chat);
        done.done("Told them", now()).unwrap();
        let block = waiting_block(
            &[
                kept(at_eight()),
                done,
                kept(WhenDraft::Place {
                    label: "home".into(),
                    on: "arrive".into(),
                }),
            ],
            NAIROBI,
        );
        assert_eq!(
            block,
            "Jobs you left yourself:\n\
             - push: Leave by 7:15, the summit doors open at 8 \
             [On Thursday 3 December at 07:00; until 3 December at 09:00]\n\
             - push: Leave by 7:15, the summit doors open at 8 \
             [When you get home; until 3 December at 09:00]"
        );
        assert_eq!(waiting_block(&[], NAIROBI), "");
    }

    #[test]
    fn a_waiting_job_fires_on_the_event_its_condition_names_and_no_other() {
        let arrive = |place: &str| ConditionEvent::Place {
            place_id: place.into(),
            transition: Transition::Arrive,
        };
        let seen = |event: &str| ConditionEvent::Thing {
            source_id: "camera:gate".into(),
            event: event.into(),
        };
        let turn = |member: &str| ConditionEvent::Chat {
            profile_id: member.into(),
        };

        let at_office = kept(WhenDraft::Place {
            label: "office".into(),
            on: "arrive".into(),
        });
        assert!(fires_on(&at_office, &arrive("place-1")));
        assert!(!fires_on(&at_office, &arrive("place-2")));
        assert!(!fires_on(
            &at_office,
            &ConditionEvent::Place {
                place_id: "place-1".into(),
                transition: Transition::Leave
            }
        ));
        assert!(!fires_on(&at_office, &turn("jerry")));

        let on_person = kept(WhenDraft::Thing {
            label: "gate camera".into(),
            event: "person".into(),
        });
        assert!(fires_on(&on_person, &seen("person")));
        assert!(!fires_on(&on_person, &seen("motion")));
        assert!(!fires_on(&on_person, &arrive("place-1")));

        let on_chat = kept(WhenDraft::Chat);
        assert!(fires_on(&on_chat, &turn("jerry")));
        assert!(!fires_on(&on_chat, &turn("amina")));
        assert!(!fires_on(&on_chat, &seen("person")));

        let timed = kept(at_eight());
        for event in [arrive("place-1"), seen("person"), turn("jerry")] {
            assert!(!fires_on(&timed, &event), "{event:?}");
        }

        let mut settled = kept(WhenDraft::Chat);
        settled.expire(now() + Duration::days(40)).unwrap();
        assert!(!fires_on(&settled, &turn("jerry")));
    }

    #[test]
    fn a_settled_job_is_one_plain_line_in_the_members_log() {
        let mut told = kept(at_eight());
        told.done("sent", now()).unwrap();
        assert_eq!(
            settled_line(&told, " Sent to\nyour phone at 07:00. "),
            "Told you: Leave by 7:15, the summit doors open at 8. Sent to your phone at 07:00."
        );
        assert_eq!(
            settled_line(&told, ""),
            "Told you: Leave by 7:15, the summit doors open at 8"
        );

        let mut late = kept(WhenDraft::Chat);
        late.expire(now() + Duration::days(40)).unwrap();
        assert_eq!(
            settled_line(&late, ""),
            "Let go, its time ran out: Leave by 7:15, the summit doors open at 8"
        );
        let mut withdrawn = kept(WhenDraft::Chat);
        withdrawn.withdraw(now()).unwrap();
        assert!(settled_line(&withdrawn, "").starts_with("Withdrawn: "));

        for (act, lead) in [
            (IntentAct::Refresh, "Brought up to date: "),
            (IntentAct::LookInto, "Looked into: "),
            (IntentAct::Ask, "Asked you: "),
            (IntentAct::ProposeTodo, "Offered you a to-do: "),
        ] {
            let mut job = kept(WhenDraft::Chat);
            job.act = act;
            job.text = "Is the pool open on Friday?".into();
            job.done("ok", now()).unwrap();
            assert_eq!(
                settled_line(&job, "It opens at 6am."),
                format!("{lead}Is the pool open on Friday? It opens at 6am.")
            );
        }
        let long = settled_line(&told, &"word ".repeat(200));
        assert_eq!(long.chars().count(), MAX_LINE_CHARS);
    }
}
