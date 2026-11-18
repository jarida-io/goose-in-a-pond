//! Idea cards: the three prompts of a `cards` job, what they are shown, how their replies are
//! read and checked against docs/architecture/idea-cards.md section 1, and how a set is picked.

use std::collections::HashMap;
use std::sync::LazyLock;

use chrono::{DateTime, Datelike, NaiveDateTime, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::shared::domain::todo::{Todo, Tz};
use crate::user_data::domain::anchor::Anchor;
use crate::user_data::domain::card::{
    ActionKind, Card, CardAction, CardAnchor, CardKind, CardNotes, CardQuestion, CardSource,
    CardSourceKind, CardState, Why, WhySource, MAX_TITLE_CHARS,
};
use crate::user_data::domain::engine_job::{EngineJob, JobClass, JobKind, JobState};
use crate::user_data::domain::fact_sheet::FactLine;
use crate::user_data::domain::interest::{topic_key, Interest};
use crate::user_data::domain::memory::MemoryFragment;
use crate::user_data::domain::question::QuestionDraft;
use crate::user_data::ports::engine_jobs::EngineJobRepository;
use crate::user_data::services::engine::recipes::when_of;
use crate::user_data::services::model_json::{list_in, object_in, text_of};

pub const MAX_QUERIES: usize = 3;
pub const MAX_CANDIDATES: usize = 12;
pub const MIN_SET: usize = 5;
pub const MAX_SET: usize = 8;
pub const MAX_PER_KIND: usize = 3;
/// The member-local hour the morning set is made.
pub const MORNING_HOUR: u32 = 6;
/// Days of earlier cards the maker is told of, so it does not repeat them.
pub const RECENT_DAYS: i64 = 7;
/// Unsaved cards are deleted after this many days; a day past [`RECENT_DAYS`].
pub const KEEP_DAYS: i64 = 8;
pub const JOB_SUBJECT: &str = "Ideas for you";

const MAX_FACTS: usize = 40;
const MAX_PLANS: usize = 8;
const MAX_TODOS: usize = 10;
const MAX_INTERESTS: usize = 10;
const MAX_MEMORIES: usize = 15;
const SOURCE_CHARS: usize = 300;
const QUERY_CHARS: usize = 80;

/// Fixed so it stays in the KV prefix.
pub const QUERIES_SYSTEM: &str = "\
You choose web searches that would help Goose suggest concrete, interest-based \
ideas for one member of this household. You are given today's date, where they \
live, and what is known about them: facts, goals, to-dos and interests.

Step 1: Pick up to 3 active interests or goals that need fresh information.
Step 2: Write targeted searches for real, current things: an event this week \
near where they live, opening times or availability for a goal, or something \
new in one of their interests. Name the town or date when it matters.

Reply with JSON and nothing else:

{\"queries\": [\"<a web search>\"]}

- At most 3 searches, each under 80 characters. An empty list when nothing needs searching.
- Never put the member's name or anything private in a search.
- No explanation, no preamble.";

/// Fixed so it stays in the KV prefix. It shows no worked card: a small model copies one.
pub const MAKER_SYSTEM: &str = "\
You write structured, interest-based idea cards for one member of this \
household: something they could do, the next step on a goal, a reminder worth \
adding, or a question they could ask Goose. You are given today's date, \
numbered sources, what the member does not want, and cards shown lately.

Work step by step for each card:
Step 1 — Grounding: Pick a specific interest, goal, or fact from the numbered \
sources. Every name, date, place, and time must come from the sources, never \
from your own knowledge.
Step 2 — Concrete Idea: Name a specific, actionable thing (never generic like \
\"Next step for reading\"). Explain why it fits them (\"why\" starting with \"You\").
Step 3 — Structured Action: Attach a clear action (todo, reminder, chat, watch, \
or go_deeper) and honest caveats.

Reply with JSON and nothing else:

{\"cards\": [{\"kind\": \"<do, next_step, reminder or ask>\", \"title\": \"<concrete idea naming the thing>\", \
\"why\": \"<You ...>\", \"why_source\": <source number>, \"when\": \"<when, or empty>\", \
\"where\": \"<where, or empty>\", \"effort\": \"<time it takes, or empty>\", \
\"action\": {\"kind\": \"<todo, reminder, chat, watch or go_deeper>\", \
\"label\": \"<button words>\", \"text\": \"<to-do or reminder words>\", \"prompt\": \
\"<message to Goose>\", \"due\": \"<YYYY-MM-DD HH:MM, or empty>\", \"question\": \
{\"header\": \"<1 to 2 words>\", \"text\": \"<question to the member>\", \"options\": \
[{\"label\": \"<what they would ask Goose>\", \"description\": \"<what Goose then does>\"}]}}, \
\"sources\": [<source numbers>], \"found\": \"<what the sources say>\", \"next\": \"<what Goose would do next>\", \
\"caveats\": \"<what is unsure>\"}]}

- At most 12 cards, each different. A few good cards beat many weak ones.
- \"title\": at most 70 characters, naming a concrete entity, place, or action.
- \"why\": one line to the member starting with \"You\" that names the interest, goal, or fact it builds on.
- \"sources\": the number of every source the card uses, at least one.
- Each card has a when, a where, an effort, or an action. Most have an action.
- Actions: todo adds a to-do, reminder adds a reminder at due, chat sends the prompt to \
Goose, watch monitors a page from the sources for updates toward a goal, go_deeper has Goose research it.
- A do card names one specific event, place or activity, with its when and where.
- A reminder or todo card adds something new that follows from a goal, a memory or the web; \
never one the member's to-dos already hold, and never one due before today.
- An ask card's action is chat, with a question to the member and 2 to 4 options worded as \
things they would ask Goose; never \"Other\". Other cards have no question.
- A next_step card moves one of the member's goals forward.
- Leave out what the member does not want and what they were shown lately.
- No explanation, no preamble.";

/// Fixed so it stays in the KV prefix.
pub const CRITIC_SYSTEM: &str = "\
You check idea cards adversarially on both grounds before Goose shows them to \
one member of this household. You are given the numbered sources, what the \
member does not want, and the numbered cards.

Drop a card when either ground fails:
Ground 1 (Grounding & Relevance):
- its sources do not support what it says;
- it is not grounded in the member's interests, goals, or facts;
- it goes against what the member does not want.
Ground 2 (Concrete Quality & Uniqueness):
- it is vague or generic (no concrete entity, place, or clear thing to do);
- it repeats another card or duplicates an existing to-do;
- its action or question is broken or unhelpful.

Reply with JSON and nothing else:

{\"drop\": [<the numbers of the cards to drop>]}

- Use only the card numbers you were given. An empty list when every card is worth showing.
- No explanation, no preamble.";

const TODAY_HEADING: &str = "Today:";
const SOURCES_HEADING: &str = "Sources:";
const DONT_HEADING: &str = "What the member does not want:";
const RECENT_HEADING: &str = "Cards they were shown lately:";
const CARDS_HEADING: &str = "Cards to check:";
const NOTHING: &str = "(nothing)";

/// One thing a card may rest on, numbered from 1 in the order shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Numbered {
    pub kind: CardSourceKind,
    /// What the card's `why` and `sources` name it as.
    pub label: String,
    pub text: String,
    #[serde(default)]
    pub url: Option<String>,
    /// The anchor's id for a plan; empty otherwise.
    #[serde(default)]
    pub reference: String,
}

/// What a set's sources are read from, before the web.
pub struct SourceRows<'a> {
    pub facts: &'a [FactLine],
    pub plans: &'a [Anchor],
    pub todos: &'a [Todo],
    pub interests: &'a [Interest],
    pub memories: &'a [MemoryFragment],
}

/// One web passage a search kept.
pub struct Passage {
    pub title: String,
    pub url: String,
    pub text: String,
}

/// Why a drafted card was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardRefused {
    NoTitle,
    UnknownKind,
    NoWhy,
    NoSource,
    /// It cites a number the maker was never shown.
    UnknownSource,
    /// It has none of when/where, cost/effort or an action.
    NothingToGoOn,
    UnknownAction,
    /// A to-do or reminder resting only on a to-do the member already has.
    RepeatsTodo,
    /// Its to-do or reminder is due in the past.
    DuePassed,
    /// An ask card with no question for Goose.
    NoQuestion,
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` on one line, cut at a word to `max` characters.
fn clipped(text: &str, max: usize) -> String {
    let line = one_line(text);
    if line.chars().count() <= max {
        return line;
    }
    let cut: String = line.chars().take(max).collect();
    match cut.rfind(' ') {
        Some(at) if at > max / 2 => cut[..at].trim_end().to_string(),
        _ => cut,
    }
}

fn numbered(kind: CardSourceKind, label: &str, text: &str) -> Numbered {
    Numbered {
        kind,
        label: clipped(label, MAX_TITLE_CHARS),
        text: clipped(text, SOURCE_CHARS),
        url: None,
        reference: String::new(),
    }
}

/// The numbered sources from the member's own rows; web passages follow with [`with_web`].
pub fn sources(rows: SourceRows<'_>, timezone: &str) -> Vec<Numbered> {
    let mut out: Vec<Numbered> = Vec::new();
    out.extend(
        rows.facts
            .iter()
            .take(MAX_FACTS)
            .map(|line| numbered(CardSourceKind::Fact, &line.fact, &line.fact)),
    );
    for plan in rows.plans.iter().take(MAX_PLANS) {
        let mut text = plan.title.clone();
        if let Some(when) = when_of(plan, timezone) {
            text.push_str(&format!(", {when}"));
        }
        if let Some(place) = plan.place.as_deref() {
            text.push_str(&format!(", at {place}"));
        }
        let mut source = numbered(CardSourceKind::Plan, &plan.title, &text);
        source.reference = plan.id.clone();
        out.push(source);
    }
    for todo in rows.todos.iter().take(MAX_TODOS) {
        let mut text = todo.text.clone();
        if let Some(due) = todo.due_at {
            text.push_str(&format!(", due {}", local_words(due, timezone)));
        }
        out.push(numbered(CardSourceKind::Todo, &todo.text, &text));
    }
    out.extend(
        rows.interests
            .iter()
            .filter(|interest| interest.confirmed && !interest.avoid)
            .take(MAX_INTERESTS)
            .map(|interest| numbered(CardSourceKind::Interest, &interest.topic, &interest.topic)),
    );
    out.extend(
        rows.memories
            .iter()
            .take(MAX_MEMORIES)
            .map(|memory| numbered(CardSourceKind::Memory, &memory.content, &memory.content)),
    );
    out
}

/// `shown` with the web passages numbered after it.
pub fn with_web(mut shown: Vec<Numbered>, passages: &[Passage]) -> Vec<Numbered> {
    for passage in passages {
        let mut source = numbered(
            CardSourceKind::Web,
            &passage.title,
            &format!("{}: {}", passage.title, passage.text),
        );
        source.url = Some(passage.url.clone());
        shown.push(source);
    }
    shown
}

fn local_words(at: DateTime<Utc>, timezone: &str) -> String {
    let tz: Tz = crate::shared::domain::todo::zone(timezone);
    at.with_timezone(&tz)
        .format("%A %-d %B %Y %H:%M")
        .to_string()
}

/// The date line every prompt opens with, and the place when the household named one.
pub fn today_line(now: DateTime<Utc>, timezone: &str, place: Option<&str>) -> String {
    let mut line = format!("It is {}.", local_words(now, timezone));
    if let Some(place) = place.filter(|place| !place.trim().is_empty()) {
        line.push_str(&format!(" The member lives in {place}."));
    }
    line
}

fn source_lines(shown: &[Numbered]) -> String {
    if shown.is_empty() {
        return NOTHING.to_string();
    }
    shown
        .iter()
        .enumerate()
        .map(|(at, source)| {
            let mut line = format!("{}. ({}) {}", at + 1, source.kind.as_str(), source.text);
            if let Some(url) = &source.url {
                line.push_str(&format!(" [{url}]"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn or_nothing(text: &str) -> &str {
    if text.trim().is_empty() {
        NOTHING
    } else {
        text
    }
}

pub fn build_queries_message(today: &str, shown: &[Numbered]) -> String {
    format!(
        "{TODAY_HEADING}\n{today}\n\n{SOURCES_HEADING}\n{}",
        source_lines(shown)
    )
}

/// Up to [`MAX_QUERIES`] distinct searches.
pub fn parse_queries(raw: &str) -> Vec<String> {
    let mut queries: Vec<String> = Vec::new();
    for item in list_in(raw, "queries").unwrap_or_default() {
        let Some(query) = item.as_str().map(|q| clipped(q, QUERY_CHARS)) else {
            continue;
        };
        let fresh = !query.is_empty()
            && !query.starts_with('<')
            && !queries.iter().any(|q| q.eq_ignore_ascii_case(&query));
        if fresh && queries.len() < MAX_QUERIES {
            queries.push(query);
        }
    }
    queries
}

pub fn build_maker_message(
    today: &str,
    shown: &[Numbered],
    dont: &str,
    recent_titles: &[String],
) -> String {
    let recent = if recent_titles.is_empty() {
        NOTHING.to_string()
    } else {
        recent_titles
            .iter()
            .map(|title| format!("- {title}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "{TODAY_HEADING}\n{today}\n\n{SOURCES_HEADING}\n{}\n\n{DONT_HEADING}\n{}\n\n\
         {RECENT_HEADING}\n{recent}",
        source_lines(shown),
        or_nothing(dont)
    )
}

fn number_of(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(number) => number.as_i64(),
        serde_json::Value::String(text) => text.trim().trim_matches(['[', ']']).parse().ok(),
        _ => None,
    }
}

/// The 0-based index of a cited source number; `Err` for one the maker was never shown.
fn cited(number: i64, shown: &[Numbered]) -> Result<usize, CardRefused> {
    usize::try_from(number)
        .ok()
        .filter(|n| (1..=shown.len()).contains(n))
        .map(|n| n - 1)
        .ok_or(CardRefused::UnknownSource)
}

fn default_label(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::Todo => "Add to-do",
        ActionKind::Reminder => "Remind me",
        ActionKind::Chat => "Ask Goose",
        ActionKind::Watch => "Watch the page",
        ActionKind::GoDeeper => "Go deeper",
    }
}

type Item = serde_json::Map<String, serde_json::Value>;

fn action_of(
    item: &Item,
    title: &str,
    web_url: Option<&String>,
    for_plan: bool,
) -> Result<Option<CardAction>, CardRefused> {
    let Some(raw) = item.get("action").filter(|a| !a.is_null()) else {
        return Ok(None);
    };
    let (kind, fields) = match raw {
        serde_json::Value::Object(fields) => match text_of(fields, "kind") {
            None => return Ok(None),
            Some(kind) => (kind, Some(fields)),
        },
        serde_json::Value::String(kind) if kind.trim().is_empty() => return Ok(None),
        serde_json::Value::String(kind) => (kind.clone(), None),
        _ => return Err(CardRefused::UnknownAction),
    };
    let kind = ActionKind::parse(&kind).ok_or(CardRefused::UnknownAction)?;
    let field = |key: &str| fields.and_then(|f| text_of(f, key));
    let mut action = CardAction {
        kind,
        label: field("label")
            .map(|label| clipped(&label, 30))
            .unwrap_or_else(|| default_label(kind).to_string()),
        prompt: None,
        text: None,
        due: None,
        url: web_url.cloned(),
        question: None,
    };
    match kind {
        ActionKind::Todo | ActionKind::Reminder => {
            action.text = Some(field("text").unwrap_or_else(|| title.to_string()));
            action.due = field("due").filter(|due| due_time(due, chrono_tz::UTC).is_some());
        }
        ActionKind::Chat => {
            action.prompt = Some(field("prompt").unwrap_or_else(|| title.to_string()));
            action.question = fields
                .and_then(|f| f.get("question"))
                .and_then(QuestionDraft::read)
                .and_then(|draft| draft.checked().ok())
                .map(CardQuestion::from);
        }
        // Nothing to watch but a page the sources gave, for a plan the member is following.
        ActionKind::Watch if web_url.is_none() || !for_plan => return Ok(None),
        ActionKind::Watch | ActionKind::GoDeeper => {}
    }
    Ok(Some(action))
}

/// One drafted card, checked against section 1 of the spec.
pub fn validate(
    item: &Item,
    shown: &[Numbered],
    profile_id: &str,
    batch_id: &str,
    now: DateTime<Utc>,
) -> Result<Card, CardRefused> {
    let title = text_of(item, "title")
        .map(|title| clipped(&title, MAX_TITLE_CHARS))
        .ok_or(CardRefused::NoTitle)?;
    let kind = text_of(item, "kind")
        .and_then(|kind| CardKind::parse(&kind))
        .ok_or(CardRefused::UnknownKind)?;
    let why_text = text_of(item, "why").ok_or(CardRefused::NoWhy)?;
    let numbers: Vec<i64> = item
        .get("sources")
        .and_then(|sources| sources.as_array())
        .map(|sources| sources.iter().filter_map(number_of).collect())
        .unwrap_or_default();
    if numbers.is_empty() {
        return Err(CardRefused::NoSource);
    }
    let mut picked: Vec<usize> = Vec::new();
    for number in numbers {
        let at = cited(number, shown)?;
        if !picked.contains(&at) {
            picked.push(at);
        }
    }
    let why_at = match item.get("why_source").and_then(number_of) {
        Some(number) => cited(number, shown)?,
        None => picked[0],
    };
    let web_url = picked.iter().find_map(|at| shown[*at].url.as_ref());
    let for_plan = picked
        .iter()
        .any(|at| shown[*at].kind == CardSourceKind::Plan);
    let mut action = action_of(item, &title, web_url, for_plan)?;
    if let Some(action) = &action {
        let adds = matches!(action.kind, ActionKind::Todo | ActionKind::Reminder);
        let only_todos = picked
            .iter()
            .all(|at| shown[*at].kind == CardSourceKind::Todo);
        if adds && only_todos {
            return Err(CardRefused::RepeatsTodo);
        }
        let due = action
            .due
            .as_deref()
            .and_then(|d| due_time(d, chrono_tz::UTC));
        if adds && due.is_some_and(|due| due < now) {
            return Err(CardRefused::DuePassed);
        }
    }
    if kind == CardKind::Ask {
        let asks = action
            .as_ref()
            .is_some_and(|a| a.kind == ActionKind::Chat && a.question.is_some());
        if !asks {
            return Err(CardRefused::NoQuestion);
        }
    } else if let Some(action) = action.as_mut() {
        action.question = None;
    }
    let field = |key: &str| text_of(item, key).map(|text| clipped(&text, 60));
    let (when, where_, cost, effort) = (
        field("when"),
        field("where"),
        field("cost"),
        field("effort"),
    );
    let groups = (when.is_some() || where_.is_some())
        || (cost.is_some() || effort.is_some())
        || action.is_some();
    if !groups {
        return Err(CardRefused::NothingToGoOn);
    }
    let why_source = &shown[why_at];
    let anchor = picked
        .iter()
        .map(|at| &shown[*at])
        .find(|source| source.kind == CardSourceKind::Plan && !source.reference.is_empty())
        .map(|plan| CardAnchor {
            id: plan.reference.clone(),
            title: plan.label.clone(),
        });
    let note = |key: &str| text_of(item, key).unwrap_or_default();
    Ok(Card {
        id: uuid::Uuid::new_v4().to_string(),
        profile_id: profile_id.to_string(),
        kind,
        title,
        why: Why {
            text: clipped(&why_text, 200),
            source: WhySource {
                kind: why_source.kind,
                label: why_source.label.clone(),
                url: why_source.url.clone(),
            },
        },
        when,
        where_,
        cost,
        effort,
        action,
        sources: picked
            .iter()
            .map(|at| CardSource {
                kind: shown[*at].kind,
                title: shown[*at].label.clone(),
                url: shown[*at].url.clone(),
            })
            .collect(),
        notes: CardNotes {
            found: note("found"),
            next: note("next"),
            caveats: note("caveats"),
        },
        anchor,
        state: CardState::New,
        batch_id: batch_id.to_string(),
        rank: 0,
        created_at: now,
        fades_at: Card::fade_time(now),
    })
}

/// What a maker's reply came to: the cards that passed, in its order, and why the rest did not.
pub struct Drafted {
    pub cards: Vec<Card>,
    pub refused: Vec<CardRefused>,
}

pub fn parse_cards(
    raw: &str,
    shown: &[Numbered],
    profile_id: &str,
    batch_id: &str,
    now: DateTime<Utc>,
) -> Drafted {
    let items = list_in(raw, "cards")
        .or_else(|| object_in(raw).map(|one| vec![serde_json::Value::Object(one)]))
        .unwrap_or_default();
    let mut drafted = Drafted {
        cards: Vec::new(),
        refused: Vec::new(),
    };
    for item in items.iter().take(MAX_CANDIDATES) {
        let Some(item) = item.as_object() else {
            continue;
        };
        match validate(item, shown, profile_id, batch_id, now) {
            Ok(card) => drafted.cards.push(card),
            Err(why) => drafted.refused.push(why),
        }
    }
    drafted
}

fn card_line(at: usize, card: &Card) -> String {
    let mut line = format!(
        "Card {}: ({}) {}. Why: {}.",
        at + 1,
        card.kind.as_str(),
        card.title,
        card.why.text
    );
    for (label, value) in [
        ("When", &card.when),
        ("Where", &card.where_),
        ("Cost", &card.cost),
        ("Effort", &card.effort),
    ] {
        if let Some(value) = value {
            line.push_str(&format!(" {label}: {value}."));
        }
    }
    if let Some(action) = &card.action {
        line.push_str(&format!(" Action: {}.", action.label));
    }
    let titles: Vec<&str> = card.sources.iter().map(|s| s.title.as_str()).collect();
    line.push_str(&format!(" Sources: {}.", titles.join("; ")));
    line
}

pub fn build_critic_message(today: &str, shown: &[Numbered], dont: &str, cards: &[Card]) -> String {
    let lines: Vec<String> = cards
        .iter()
        .enumerate()
        .map(|(at, card)| card_line(at, card))
        .collect();
    format!(
        "{TODAY_HEADING}\n{today}\n\n{SOURCES_HEADING}\n{}\n\n{DONT_HEADING}\n{}\n\n\
         {CARDS_HEADING}\n{}",
        source_lines(shown),
        or_nothing(dont),
        lines.join("\n")
    )
}

/// The 0-based cards the critic dropped; `None` when its reply cannot be read.
pub fn parse_drops(raw: &str, count: usize) -> Option<Vec<usize>> {
    let items = list_in(raw, "drop")?;
    let mut drops: Vec<usize> = items
        .iter()
        .filter_map(number_of)
        .filter_map(|n| usize::try_from(n).ok())
        .filter(|n| (1..=count).contains(n))
        .map(|n| n - 1)
        .collect();
    drops.sort_unstable();
    drops.dedup();
    Some(drops)
}

fn score(card: &Card) -> usize {
    let mut score = card.sources.len().min(3);
    if card.action.is_some() {
        score += 3;
    }
    if card.when.is_some() || card.where_.is_some() {
        score += 2;
    }
    if card.cost.is_some() || card.effort.is_some() {
        score += 1;
    }
    if card.sources.iter().any(|s| s.kind == CardSourceKind::Web) {
        score += 1;
    }
    if card.kind == CardKind::Do && card.when.is_some() {
        score += 2;
    }
    score
}

/// The set to show: best first, at most [`MAX_SET`], at most [`MAX_PER_KIND`] of one kind, a
/// `next_step` when the member has an active plan, and nothing they were shown lately.
pub fn pick(cards: Vec<Card>, has_active_plan: bool, recent_titles: &[String]) -> Vec<Card> {
    let mut seen: Vec<String> = recent_titles.iter().map(|t| topic_key(t)).collect();
    let mut fresh: Vec<Card> = Vec::new();
    for card in cards {
        let key = topic_key(&card.title);
        if !key.is_empty() && !seen.contains(&key) {
            seen.push(key);
            fresh.push(card);
        }
    }
    // Stable, so the maker's own order breaks a tie.
    fresh.sort_by_key(|card| std::cmp::Reverse(score(card)));
    let mut chosen: Vec<usize> = Vec::new();
    if has_active_plan {
        chosen.extend(fresh.iter().position(|c| c.kind == CardKind::NextStep));
    }
    let mut per_kind: HashMap<CardKind, usize> = HashMap::new();
    for at in &chosen {
        *per_kind.entry(fresh[*at].kind).or_default() += 1;
    }
    for (at, card) in fresh.iter().enumerate() {
        if chosen.len() == MAX_SET {
            break;
        }
        let count = per_kind.entry(card.kind).or_default();
        if chosen.contains(&at) || *count == MAX_PER_KIND {
            continue;
        }
        *count += 1;
        chosen.push(at);
    }
    chosen.sort_unstable();
    let mut set: Vec<Card> = fresh
        .into_iter()
        .enumerate()
        .filter(|(at, _)| chosen.contains(at))
        .map(|(_, card)| card)
        .collect();
    for (rank, card) in set.iter_mut().enumerate() {
        card.rank = rank as u32;
    }
    set
}

/// A member-local `YYYY-MM-DD HH:MM` (or RFC 3339) as a time.
pub fn due_time(due: &str, tz: Tz) -> Option<DateTime<Utc>> {
    let due = due.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(due) {
        return Some(at.with_timezone(&Utc));
    }
    let naive = NaiveDateTime::parse_from_str(due, "%Y-%m-%d %H:%M").ok()?;
    tz.from_local_datetime(&naive)
        .earliest()
        .map(|at| at.with_timezone(&Utc))
}

/// Whether a card's `when`, in words, names the member's today.
pub fn falls_today(when: &str, tz: Tz, now: DateTime<Utc>) -> bool {
    let words = when.to_lowercase();
    if words.contains("tomorrow") {
        return false;
    }
    if [
        "today",
        "tonight",
        "this morning",
        "this afternoon",
        "this evening",
    ]
    .iter()
    .any(|word| words.contains(word))
    {
        return true;
    }
    let local = now.with_timezone(&tz);
    let weekday = local.format("%A").to_string().to_lowercase();
    if words.contains(&weekday) && !words.contains("next") {
        return true;
    }
    let (day, month, short) = (
        local.day(),
        local.format("%B").to_string().to_lowercase(),
        local.format("%b").to_string().to_lowercase(),
    );
    [
        format!("{day} {month}"),
        format!("{month} {day}"),
        format!("{day} {short}"),
        format!("{short} {day}"),
        local.format("%Y-%m-%d").to_string(),
    ]
    .iter()
    .any(|date| {
        words.match_indices(date.as_str()).any(|(at, _)| {
            let after = words[at + date.len()..].chars().next();
            !after.is_some_and(|c| c.is_ascii_digit())
        })
    })
}

/// A card worth a push: it has an action and its `when` falls today.
pub fn is_timely(card: &Card, tz: Tz, now: DateTime<Utc>) -> bool {
    card.action.is_some()
        && card
            .when
            .as_deref()
            .is_some_and(|when| falls_today(when, tz, now))
}

/// Whether the morning set is due: past 06:00 member-local, no set made since then, and none
/// being made.
pub fn morning_set_due(
    last_finished: Option<DateTime<Utc>>,
    one_open: bool,
    tz: Tz,
    now: DateTime<Utc>,
) -> bool {
    if one_open {
        return false;
    }
    let local = now.with_timezone(&tz);
    if local.hour() < MORNING_HOUR {
        return false;
    }
    let Some(morning) = local
        .date_naive()
        .and_hms_opt(MORNING_HOUR, 0, 0)
        .and_then(|at| tz.from_local_datetime(&at).earliest())
        .map(|at| at.with_timezone(&Utc))
    else {
        return false;
    };
    last_finished.is_none_or(|at| at < morning)
}

/// Serialises "has this member an open set?" with queueing one, so a set is never made twice
/// at once.
static REQUEST_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Queues a set for the member, or returns the one already open, moved up to `class` while it
/// still waits.
pub async fn request_cards(
    jobs: &dyn EngineJobRepository,
    profile_id: &str,
    class: JobClass,
    now: DateTime<Utc>,
) -> anyhow::Result<EngineJob> {
    let _held = REQUEST_LOCK.lock().await;
    let open = jobs.open_of_kind(profile_id, JobKind::Cards).await?;
    let Some(mut job) = open.into_iter().next() else {
        let job = EngineJob::new(JobKind::Cards, class, Some(profile_id), JOB_SUBJECT, now);
        jobs.add(&job).await?;
        return Ok(job);
    };
    if job.state == JobState::Queued && class < job.class {
        job.class = class;
        jobs.save(&job).await?;
    }
    Ok(job)
}

/// How many of the Pond's engine jobs run before `waiting`: those already running, and those queued
/// before it. The engine has one inference lane for the whole household, so a set asked for while
/// three plans are being researched waits for all three; this is the number a member is told, so a
/// refresh that is only queued does not read as stuck. Another member's jobs count, as they share the
/// lane; only the number is told, never what they are.
pub fn jobs_ahead(open: &[EngineJob], waiting: &EngineJob) -> usize {
    open.iter()
        .filter(|job| job.id != waiting.id)
        .filter(|job| match job.state {
            JobState::Running => true,
            JobState::Queued => (job.created_at, &job.id) < (waiting.created_at, &waiting.id),
            _ => false,
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::mocks::mock_engine_jobs::InMemoryEngineJobRepository;
    use chrono::Duration;
    use serde_json::json;

    const NAIROBI: Tz = chrono_tz::Africa::Nairobi;

    // 2026-11-18: a refresh after Go deeper on three cards sat behind ten jobs with only a spinner.
    #[test]
    fn a_waiting_set_counts_what_runs_and_what_was_queued_before_it() {
        let at = |minutes: i64| now() + Duration::minutes(minutes);
        let job = |kind, minutes, member| {
            EngineJob::new(kind, JobClass::Asked, Some(member), "x", at(minutes))
        };
        let mut running = job(JobKind::Deliverable, 0, "liz");
        running.start(None, at(1)).unwrap();
        let earlier = job(JobKind::Questions, 2, "jerry");
        let set = job(JobKind::Cards, 5, "jerry");
        let later = job(JobKind::Deliverable, 9, "jerry");
        let mut done = job(JobKind::FactSheet, 1, "jerry");
        done.state = JobState::Passed;

        let open = [running, earlier, set.clone(), later, done];
        assert_eq!(
            jobs_ahead(&open, &set),
            2,
            "the running job and the one queued first"
        );
        assert_eq!(jobs_ahead(&[set.clone()], &set), 0);
    }

    fn now() -> DateTime<Utc> {
        // Tuesday 17 November 2026, 09:00 in Nairobi.
        Utc.with_ymd_and_hms(2026, 11, 17, 6, 0, 0).unwrap()
    }

    fn shown() -> Vec<Numbered> {
        let mut plan = numbered(
            CardSourceKind::Plan,
            "AI Summit Nairobi",
            "AI Summit, Saturday",
        );
        plan.reference = "anchor-1".to_string();
        let mut web = numbered(
            CardSourceKind::Web,
            "Karura Forest",
            "Karura Forest: KSh 600",
        );
        web.url = Some("https://karura.example/".to_string());
        vec![
            numbered(CardSourceKind::Fact, "Runs on weekends", "Runs on weekends"),
            plan,
            web,
        ]
    }

    fn item(value: serde_json::Value) -> Item {
        value.as_object().unwrap().clone()
    }

    fn check(value: serde_json::Value) -> Result<Card, CardRefused> {
        validate(&item(value), &shown(), "jerry", "batch", now())
    }

    fn good() -> serde_json::Value {
        json!({
            "kind": "do", "title": "Run the Karura loop on Saturday",
            "why": "You run on weekends", "why_source": 1,
            "when": "Saturday 7 am", "where": "Karura Forest", "cost": "KSh 600",
            "action": {"kind": "todo", "label": "Add to-do"},
            "sources": [1, 3], "found": "Entry is KSh 600", "next": "", "caveats": ""
        })
    }

    #[test]
    fn a_card_with_its_why_sources_and_a_detail_passes_with_what_it_rests_on() {
        let card = check(good()).unwrap();
        assert_eq!(card.why.source.kind, CardSourceKind::Fact);
        assert_eq!(card.sources.len(), 2);
        assert_eq!(
            card.sources[1].url.as_deref(),
            Some("https://karura.example/")
        );
        let action = card.action.unwrap();
        assert_eq!(
            action.text.as_deref(),
            Some("Run the Karura loop on Saturday")
        );
        assert_eq!(card.fades_at, now() + Duration::hours(36));
        assert!(card.anchor.is_none());

        let mut planned = good();
        planned["sources"] = json!([2]);
        planned["kind"] = json!("next_step");
        let card = check(planned).unwrap();
        assert_eq!(card.anchor.unwrap().id, "anchor-1");
    }

    #[test]
    fn the_code_refuses_what_section_one_forbids() {
        let without = |key: &str| {
            let mut card = good();
            card.as_object_mut().unwrap().remove(key);
            card
        };
        assert_eq!(check(without("why")).unwrap_err(), CardRefused::NoWhy);
        assert_eq!(
            check(without("sources")).unwrap_err(),
            CardRefused::NoSource
        );
        assert_eq!(check(without("title")).unwrap_err(), CardRefused::NoTitle);

        let mut unknown = good();
        unknown["sources"] = json!([1, 9]);
        assert_eq!(check(unknown).unwrap_err(), CardRefused::UnknownSource);
        let mut unknown_why = good();
        unknown_why["why_source"] = json!(0);
        assert_eq!(check(unknown_why).unwrap_err(), CardRefused::UnknownSource);

        let mut bare = json!({"kind": "do", "title": "Run", "why": "You run", "sources": [1]});
        assert_eq!(check(bare.clone()).unwrap_err(), CardRefused::NothingToGoOn);
        bare["action"] = json!({"kind": "call"});
        assert_eq!(check(bare.clone()).unwrap_err(), CardRefused::UnknownAction);
        bare["action"] = json!({"kind": "watch"});
        assert_eq!(
            check(bare).unwrap_err(),
            CardRefused::NothingToGoOn,
            "a watch with no page from the sources is no action"
        );
        let mut idea = good();
        idea["kind"] = json!("idea");
        assert_eq!(check(idea).unwrap_err(), CardRefused::UnknownKind);
    }

    #[test]
    fn reminders_repeating_a_todo_or_already_past_are_refused() {
        let mut todo = numbered(CardSourceKind::Todo, "Wash the dishes", "Wash the dishes");
        todo.reference = "todo-1".to_string();
        let mut with_todo = shown();
        with_todo.push(todo);
        let checked =
            |value: serde_json::Value| validate(&item(value), &with_todo, "jerry", "batch", now());
        let mut repeat = good();
        repeat["kind"] = json!("reminder");
        repeat["sources"] = json!([4]);
        repeat["why_source"] = json!(4);
        repeat["action"] = json!({"kind": "reminder", "text": "Wash the dishes"});
        assert_eq!(checked(repeat).unwrap_err(), CardRefused::RepeatsTodo);

        let mut past = good();
        past["action"] = json!({"kind": "reminder", "due": "2026-11-13 19:00"});
        assert_eq!(checked(past).unwrap_err(), CardRefused::DuePassed);
        let mut ahead = good();
        ahead["action"] = json!({"kind": "reminder", "due": "2026-11-21 07:00"});
        assert!(checked(ahead).is_ok());
    }

    #[test]
    fn an_ask_card_needs_a_question_and_a_watch_needs_a_plan() {
        let mut ask = good();
        ask["kind"] = json!("ask");
        ask["action"] = json!({"kind": "chat", "prompt": "What's on at Karura this weekend?"});
        assert_eq!(
            check(ask.clone()).unwrap_err(),
            CardRefused::NoQuestion,
            "a prompt alone is no structured question"
        );
        ask["action"]["question"] = json!({"header": "Saturday", "text": "What should Goose look into?",
            "options": ["What's on at Karura", "Other"]});
        assert_eq!(
            check(ask.clone()).unwrap_err(),
            CardRefused::NoQuestion,
            "an option meaning Other refuses it"
        );
        ask["action"]["question"] = json!({"header": "Saturday", "text": "What should Goose look into?",
            "options": [{"label": "What's on at Karura", "description": "Events this weekend"},
                        {"label": "Kids' things nearby", "description": "Within 20 minutes"}],
            "recommended": "What's on at Karura"});
        let card = check(ask).unwrap();
        let question = card.action.unwrap().question.unwrap();
        assert_eq!(question.header, "Saturday");
        assert_eq!(question.recommended, Some(0));
        assert_eq!(question.options[1].description, "Within 20 minutes");

        let mut chat = good();
        chat["action"] = json!({"kind": "chat", "prompt": "Plan it?",
            "question": {"header": "Q", "text": "Which?", "options": ["A", "B"]}});
        assert_eq!(
            check(chat).unwrap().action.unwrap().question,
            None,
            "only an ask card carries a question"
        );

        let mut watch = good();
        watch["action"] = json!({"kind": "watch"});
        assert!(
            check(watch.clone()).unwrap().action.is_none(),
            "a watch on a page for no plan is dropped"
        );
        watch["sources"] = json!([2, 3]);
        let card = check(watch).unwrap();
        assert_eq!(card.action.unwrap().kind, ActionKind::Watch);
    }

    #[test]
    fn a_reply_is_read_leniently_and_a_long_title_is_cut() {
        let mut long = good();
        long["title"] = json!("word ".repeat(30));
        let reply = format!(
            "```json\n{}\n```",
            json!({"cards": [good(), long, {"kind": "do"}]})
        );
        let drafted = parse_cards(&reply, &shown(), "jerry", "batch", now());
        assert_eq!(drafted.cards.len(), 2);
        assert_eq!(drafted.refused, vec![CardRefused::NoTitle]);
        assert!(drafted.cards[1].title.chars().count() <= MAX_TITLE_CHARS);
        assert_eq!(
            parse_queries(r#"{"queries": ["a", "A", "b", "c", "d"]}"#),
            ["a", "b", "c"]
        );
        assert_eq!(parse_drops(r#"{"drop": [2, "1", 9]}"#, 3), Some(vec![0, 1]));
        assert_eq!(parse_drops("no idea", 3), None);
    }

    fn card(kind: CardKind, title: &str, action: bool) -> Card {
        let mut value = good();
        value["kind"] = json!(kind.as_str());
        value["title"] = json!(title);
        if kind == CardKind::Ask {
            value["action"] = json!({"kind": "chat", "prompt": format!("{title}?"),
                "question": {"header": "Ask", "text": format!("{title}?"),
                             "options": ["Yes, look into it", "Something nearby"]}});
        }
        if !action {
            value.as_object_mut().unwrap().remove("action");
        }
        check(value).unwrap()
    }

    #[test]
    fn a_set_keeps_eight_at_most_three_of_a_kind_and_a_next_step_for_a_plan() {
        let mut drafted: Vec<Card> = (0..6)
            .map(|n| card(CardKind::Do, &format!("Do thing {n}"), true))
            .collect();
        drafted.extend((0..5).map(|n| card(CardKind::Ask, &format!("Ask thing {n}"), true)));
        drafted
            .extend((0..3).map(|n| card(CardKind::Reminder, &format!("Remind thing {n}"), true)));
        drafted.push(card(CardKind::NextStep, "Book the summit hotel", false));
        drafted.push(card(CardKind::Do, "Do thing 0", true));

        let set = pick(drafted.clone(), true, &["Ask thing 0".to_string()]);
        let count = |kind: CardKind| set.iter().filter(|c| c.kind == kind).count();
        assert_eq!(set.len(), 8);
        assert_eq!(count(CardKind::Do), 3);
        assert_eq!(count(CardKind::Ask), 3);
        assert_eq!(count(CardKind::NextStep), 1, "kept though it scores lowest");
        assert_eq!(count(CardKind::Reminder), 1);
        assert!(set.iter().all(|c| c.title != "Ask thing 0"), "shown lately");
        assert_eq!(set.iter().filter(|c| c.title == "Do thing 0").count(), 1);
        assert!(set.iter().enumerate().all(|(at, c)| c.rank == at as u32));

        let no_plan = pick(drafted, false, &[]);
        assert_eq!(no_plan.len(), 8);
        assert_eq!(
            no_plan
                .iter()
                .filter(|c| c.kind == CardKind::NextStep)
                .count(),
            0,
            "only on its merits without a plan"
        );
    }

    #[test]
    fn a_card_is_pushed_only_when_it_falls_today_with_an_action() {
        for when in [
            "today 6 pm",
            "Tuesday evening",
            "17 November",
            "Nov 17",
            "2026-11-17",
        ] {
            assert!(falls_today(when, NAIROBI, now()), "{when}");
        }
        for when in [
            "tomorrow",
            "next Tuesday",
            "Saturday",
            "17 December",
            "Nov 1",
            "Nov 170",
        ] {
            assert!(!falls_today(when, NAIROBI, now()), "{when}");
        }
        let mut timely = card(CardKind::Do, "Run", true);
        timely.when = Some("today".to_string());
        assert!(is_timely(&timely, NAIROBI, now()));
        timely.action = None;
        assert!(!is_timely(&timely, NAIROBI, now()));
    }

    #[test]
    fn the_morning_set_is_due_once_after_six_and_never_while_one_is_open() {
        let six = Utc.with_ymd_and_hms(2026, 11, 17, 3, 0, 0).unwrap();
        assert!(!morning_set_due(
            None,
            false,
            NAIROBI,
            six - Duration::minutes(1)
        ));
        assert!(morning_set_due(None, false, NAIROBI, six));
        assert!(morning_set_due(
            Some(six - Duration::hours(2)),
            false,
            NAIROBI,
            now()
        ));
        assert!(!morning_set_due(
            Some(six + Duration::minutes(5)),
            false,
            NAIROBI,
            now()
        ));
        assert!(!morning_set_due(None, true, NAIROBI, now()));
    }

    #[tokio::test]
    async fn a_member_never_has_two_sets_queued_at_once() {
        let jobs = InMemoryEngineJobRepository::new();
        let first = request_cards(&jobs, "jerry", JobClass::Upkeep, now())
            .await
            .unwrap();
        let second = request_cards(&jobs, "jerry", JobClass::Asked, now())
            .await
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(second.class, JobClass::Asked);
        assert_eq!(jobs.open().await.unwrap().len(), 1);
    }

    #[test]
    fn the_due_of_a_reminder_is_read_on_the_members_clock() {
        assert_eq!(
            due_time("2026-11-17 18:00", NAIROBI),
            Some(Utc.with_ymd_and_hms(2026, 11, 17, 15, 0, 0).unwrap())
        );
        assert_eq!(due_time("Saturday", NAIROBI), None);
    }
}
