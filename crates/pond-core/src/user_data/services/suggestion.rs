//! Suggestions: questions the household could ask now, phrased only from facts the pond holds.
//!
//! Unlike a proposal, a suggestion stages no action (the tap is the consent), so it needs no
//! audience, expiry or daily cap. Derived on read, never stored; no suggestor spends inference.

use crate::user_data::domain::anchor::{
    Anchor, AnchorKind, AnchorState, Deliverable, DeliverableKind, DeliverableState,
};
use crate::user_data::domain::card::{Card, CardKind};
use crate::user_data::domain::fact_sheet::{rank, FactCategory, FactLine, FactState};
use crate::user_data::domain::question::{Question, QuestionPurpose, QuestionState};
use crate::user_data::domain::suggestion::SuggestionContext;
use crate::user_data::services::engine::recipes::{asked_about, when_of};
use crate::user_data::services::engine::settle::ready_notice;
use chrono::{DateTime, Datelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

// ── What the household may be shown ─────────────────────────────────────────

/// Who is looking: may personal facts be shown? Every scope but `Guest` is `Personal`, as
/// `identity_resolution::resolve` yields `Household` only on a pond of at most one member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// One member, or a household of one. Personal facts are theirs to see.
    Personal,
    /// An unidentified speaker on a multi-member pond: house facts only, never a person's.
    Shared,
}

impl Audience {
    /// Whether a suggestor drawing on one person's data may run.
    pub fn may_see_personal(self) -> bool {
        matches!(self, Audience::Personal)
    }
}

/// Tool-group names, matching `TOOL_GROUPS` in [`tool_group`](crate::mcp::domain::tool_group).
pub const GROUP_CONTEXT: &str = "giap-context";
pub const GROUP_SCHEDULE: &str = "giap-schedule";
pub const GROUP_MEMORY: &str = "giap-memory";
pub const GROUP_DEVICE: &str = "giap-device";
pub const GROUP_WEATHER: &str = "giap-weather";
pub const GROUP_KNOWLEDGE: &str = "giap-knowledge";

/// Tool names as a fact line's hints spell them.
pub const TOOL_WEATHER_FORECAST: &str = "get_weather_forecast";
pub const TOOL_WEATHER_NOW: &str = "get_current_weather";
pub const TOOL_WEB_WATCH: &str = "web_watch";
pub const TOOL_SEARCH_WEB: &str = "search_web";

/// One thing the household might want to ask.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggestion {
    /// The suggestor's id, not a hash of the fact, so a mute keeps matching as the number moves.
    pub id: String,
    /// The card's text and the prompt sent on tap, as one string so the two can't disagree.
    pub prompt: String,
    /// The fact that produced it, with the number in it. Never blank.
    pub because: String,
    /// The tool group that can answer [`Self::prompt`].
    pub answered_by: &'static str,
    /// Grounding context passed to the model when the suggestion is tapped.
    pub context: Option<SuggestionContext>,
}

/// One suggestor's outcome, returned even when silent so "broken" and "nothing to say" differ.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Considered {
    pub id: String,
    /// `None` when it offered something.
    pub silent_because: Option<String>,
}

/// What [`suggest`] produced, and what it considered.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SuggestionSet {
    pub offered: Vec<Suggestion>,
    pub considered: Vec<Considered>,
}

// ── What the caller must measure ────────────────────────────────────────────

/// What the pond knows about its tool groups; an empty report means unknown, not none.
/// The extension manager reports empty without a live agent session, e.g. no provider yet.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum GroupsKnown {
    /// The manager answered with a real list. A group not in it is absent.
    These(BTreeSet<String>),
    /// Nobody could say: offer everything; an unanswerable card costs less than a hidden feature.
    #[default]
    Unknown,
}

impl GroupsKnown {
    /// Build from an extension manager's report; an empty list is `Unknown`, not `These(empty)`.
    pub fn from_report(report: Option<Vec<String>>) -> Self {
        match report {
            Some(names) if !names.is_empty() => Self::These(names.into_iter().collect()),
            _ => Self::Unknown,
        }
    }

    fn has(&self, group: &str) -> bool {
        match self {
            Self::These(set) => set.contains(group),
            Self::Unknown => true,
        }
    }
}

/// The hinted tools this pond can use now; search only with its key, as without one it answers
/// with a setup message.
pub fn tools_ready(weather_on: bool, knowledge_on: bool, search_has_key: bool) -> BTreeSet<String> {
    let mut ready = BTreeSet::new();
    if weather_on {
        ready.insert(TOOL_WEATHER_FORECAST.to_string());
        ready.insert(TOOL_WEATHER_NOW.to_string());
    }
    if knowledge_on {
        ready.insert(TOOL_WEB_WATCH.to_string());
        if search_has_key {
            ready.insert(TOOL_SEARCH_WEB.to_string());
        }
    }
    ready
}

/// A deliverable ready for the member that they have not opened, with the anchor it was made for.
#[derive(Debug, Clone)]
pub struct UnseenWork {
    pub anchor: Anchor,
    pub deliverable: Deliverable,
}

/// Everything the suggestors read, gathered once by the caller.
/// Each field was measured; no `Option` stands in for "did not look".
#[derive(Debug, Clone, Default)]
pub struct SuggestionSnapshot {
    pub audience: Audience,
    pub groups: GroupsKnown,
    /// Suggestor ids the household has muted.
    pub muted: BTreeSet<String>,

    /// Calendar items from now to local midnight; `None` means no calendar is connected.
    pub calendar_events_today: Option<usize>,
    /// Mail items this past week (a lagging sync zeroes a daily count); `None`: no mail account.
    pub mail_items_this_week: Option<usize>,
    /// Unpaused schedules whose next run falls before local midnight.
    pub schedules_before_midnight: usize,
    /// The soonest of those, as the household's own label for it.
    pub next_schedule_label: Option<String>,
    /// Retrievable memories (active, not archived), counted as the read path counts them.
    pub active_memories: usize,
    /// Of those, the ones the extractor classified as a standing habit.
    pub routine_memories: usize,
    pub devices_registered: usize,
    /// Weather switched on AND a place resolved: either alone yields an unanswerable card.
    pub weather_ready: bool,
    /// The household's own name for where it is, when it has one.
    pub place: Option<String>,

    /// The member asking, when one is known. Only their own anchors count, but for questions.
    pub member: Option<String>,
    pub now: DateTime<Utc>,
    /// The household's IANA zone, which names the days.
    pub timezone: String,
    /// The place the household set for the pond, never one guessed from the time zone.
    pub home_place: Option<String>,
    /// The member's candidate and active anchors, their own and those shared with them.
    pub anchors: Vec<Anchor>,
    /// Work Goose finished for the member that they have not opened, newest first.
    pub unseen_work: Vec<UnseenWork>,
    /// Open questions the member may answer: theirs, and those asked for anchors shared with them.
    pub open_questions: Vec<Question>,
    /// The member's fact sheet lines.
    pub facts: Vec<FactLine>,
    /// Tool names this pond can use now, from [`tools_ready`].
    pub tools_ready: BTreeSet<String>,
}

impl Default for Audience {
    /// The narrower one, so an unfilled snapshot shows less, not more.
    fn default() -> Self {
        Audience::Shared
    }
}

// ── The suggestors ──────────────────────────────────────────────────────────

/// Every suggestor: cards about a dated anchor lead, nearest date first, and the rest keep this
/// order.
const SUGGESTOR_IDS: &[&str] = &[
    "anchor_soon",
    "questions_waiting",
    "plan_to_confirm",
    "changed_since",
    "ready_deliverable",
    "fact_hint",
    "calendar_today",
    "upcoming_schedule",
    "inbox_recent",
    "routine_recall",
    "memory_recall",
    "devices_online",
    "weather_today",
];

/// Max shown at once; more turns the Home column from a glance into a list.
pub const MAX_SUGGESTIONS: usize = 4;

/// Longest chip text, in characters; tapped, it is the member's own message.
pub const MAX_PROMPT_CHARS: usize = 90;

/// What of a deliverable's own text rides the turn its card starts; the rest is in My Goose.
const WORK_CHARS: usize = 1_500;

/// What of a refresh's list of changes rides the turn.
const CHANGED_CHARS: usize = 600;

/// The most a card's grounding carries, as a card sent to chat does.
const GROUNDING_CHARS: usize = 2_400;

/// An anchor starting within this many days, on the household's clock, is coming up.
const SOON_DAYS: i64 = 3;

/// Open questions a card's grounding lists; the rest wait in My Goose.
const QUESTIONS_LISTED: usize = 8;

/// The best fact lines the hint takes turns with, one a day.
const FACT_HINT_TURNS: usize = 3;

const ANCHOR_MARK: &str = "{anchor}";
const PLACE_MARK: &str = "{place}";

/// A suggestion, and how far the date of the anchor it is about lies from now.
struct Built {
    suggestion: Suggestion,
    /// Seconds; `None` when it is about no dated anchor.
    near: Option<i64>,
}

impl Built {
    fn undated(suggestion: Suggestion) -> Self {
        Self {
            suggestion,
            near: None,
        }
    }

    fn about(suggestion: Suggestion, anchor: &Anchor, now: DateTime<Utc>) -> Self {
        Self {
            suggestion,
            near: distance(anchor, now),
        }
    }
}

/// What a suggestion's id starts with when it is one of the member's `ask` cards.
pub const CARD_PREFIX: &str = "idea_card:";

/// The member's `ask` cards as suggestions, in the order given, ahead of `rest` and within
/// [`MAX_SUGGESTIONS`].
pub fn led_by_ask_cards(cards: &[Card], rest: Vec<Suggestion>) -> Vec<Suggestion> {
    cards
        .iter()
        .filter(|card| card.kind == CardKind::Ask)
        .map(|card| Suggestion {
            id: format!("{CARD_PREFIX}{}", card.id),
            prompt: card
                .action
                .as_ref()
                .and_then(|action| action.prompt.clone())
                .unwrap_or_else(|| card.title.clone()),
            because: card.why.text.clone(),
            answered_by: GROUP_KNOWLEDGE,
            context: None,
        })
        .chain(rest)
        .take(MAX_SUGGESTIONS)
        .collect()
}

/// Work out what this household might want to ask.
pub fn suggest(snapshot: &SuggestionSnapshot) -> SuggestionSet {
    let mut built = Vec::new();
    let mut considered = Vec::new();

    for id in SUGGESTOR_IDS {
        match run_one(id, snapshot) {
            Ok(one) => {
                considered.push(Considered {
                    id: (*id).to_string(),
                    silent_because: None,
                });
                built.push(one);
            }
            Err(reason) => considered.push(Considered {
                id: (*id).to_string(),
                silent_because: Some(reason),
            }),
        }
    }

    // Stable, so the undated keep `SUGGESTOR_IDS` order.
    built.sort_by_key(|one| (one.near.is_none(), one.near.unwrap_or(0)));
    // Cut after the loop so `considered` still records suggestors the cap left out.
    let offered = built
        .into_iter()
        .map(|one| one.suggestion)
        .take(MAX_SUGGESTIONS)
        .collect();

    SuggestionSet {
        offered,
        considered,
    }
}

/// One suggestor. `Err` carries the sentence explaining the silence.
fn run_one(id: &str, s: &SuggestionSnapshot) -> Result<Built, String> {
    if s.muted.contains(id) {
        return Err("the household muted this suggestion".to_string());
    }

    let mut built = match id {
        "anchor_soon" => anchor_soon(s),
        "questions_waiting" => questions_waiting(s),
        "plan_to_confirm" => plan_to_confirm(s),
        "changed_since" => changed_since(s),
        "ready_deliverable" => ready_deliverable(s),
        "fact_hint" => fact_hint(s),
        "calendar_today" => calendar_today(s).map(Built::undated),
        "upcoming_schedule" => upcoming_schedule(s).map(Built::undated),
        "inbox_recent" => inbox_recent(s).map(Built::undated),
        "routine_recall" => routine_recall(s).map(Built::undated),
        "memory_recall" => memory_recall(s).map(Built::undated),
        "devices_online" => devices_online(s).map(Built::undated),
        "weather_today" => weather_today(s).map(Built::undated),
        // Unreachable while SUGGESTOR_IDS matches this list (`every_suggestor_id_is_reachable`).
        other => Err(format!("no suggestor is registered under '{other}'")),
    }?;

    let answered_by = built.suggestion.answered_by;
    if !s.groups.has(answered_by) {
        return Err(format!(
            "nothing on this pond could answer it: {answered_by} is not installed"
        ));
    }
    built.suggestion.prompt = cut(&built.suggestion.prompt, MAX_PROMPT_CHARS);
    Ok(built)
}

/// The member whose engine data a suggestor reads: personal facts may be shown and one member
/// is known to be asking.
fn caller<'a>(s: &'a SuggestionSnapshot, what: &str) -> Result<&'a str, String> {
    if !s.audience.may_see_personal() {
        return Err(format!(
            "{what} is for one person, and more than one member lives here without saying who \
             is asking"
        ));
    }
    s.member
        .as_deref()
        .ok_or_else(|| "the pond does not know which member is asking".to_string())
}

fn anchor_soon(s: &SuggestionSnapshot) -> Result<Built, String> {
    let member = caller(s, "what someone has coming up")?;
    let tz = zone(s);
    let soon = s
        .anchors
        .iter()
        .filter(|a| a.profile_id == member && a.state == AnchorState::Active)
        .filter_map(|a| {
            let start = a.starts_at?;
            (0..=SOON_DAYS)
                .contains(&days_until(s.now, start, tz))
                .then_some((a, start))
        })
        .min_by_key(|(a, start)| (*start, a.created_at));
    let Some((anchor, start)) = soon else {
        return Err("nothing they are doing starts in the next three days".to_string());
    };
    let day = day_words(s.now, start, tz);
    let date = start.with_timezone(&tz).format("%-d %B");
    Ok(Built::about(
        Suggestion {
            id: "anchor_soon".to_string(),
            prompt: fitted(&anchor.title, |title| {
                format!(
                    "What do I need for {} {day}?",
                    named(anchor.kind, title, true)
                )
            }),
            because: format!("{} starts {day}, {date}.", anchor.title),
            answered_by: GROUP_MEMORY,
            context: Some(SuggestionContext::Temporal {
                event_title: anchor.title.clone(),
                timing_summary: grounding(&about_anchor(anchor, &s.timezone)),
            }),
        },
        anchor,
        s.now,
    ))
}

fn questions_waiting(s: &SuggestionSnapshot) -> Result<Built, String> {
    let member = caller(s, "a question Goose asks")?;
    let waiting = s
        .anchors
        .iter()
        .filter(|a| a.is_readable_by(member) && AnchorState::LIVE.contains(&a.state))
        .filter_map(|a| {
            let open = open_for(s, member, a);
            (!open.is_empty()).then_some((a, open))
        })
        .min_by_key(|(a, _)| nearest(a, s.now));
    let Some((anchor, open)) = waiting else {
        return Err("Goose has no open questions about anything they are doing".to_string());
    };
    let asked = plural(open.len(), "question", "questions");
    let mine = anchor.profile_id == member;
    Ok(Built::about(
        Suggestion {
            id: "questions_waiting".to_string(),
            prompt: fitted(&anchor.title, |title| {
                format!("Answer {asked} about {}", named(anchor.kind, title, mine))
            }),
            because: format!(
                "{asked} about {} {} waiting for an answer.",
                anchor.title,
                if open.len() == 1 { "is" } else { "are" }
            ),
            answered_by: GROUP_MEMORY,
            context: Some(SuggestionContext::PersonalMemory {
                source_memory_id: format!("anchor:{}", anchor.id),
                fact: grounding(&questions_brief(anchor, &open)),
            }),
        },
        anchor,
        s.now,
    ))
}

/// The open questions asked for `anchor` that `member` may answer, oldest first.
fn open_for<'a>(s: &'a SuggestionSnapshot, member: &str, anchor: &Anchor) -> Vec<&'a Question> {
    s.open_questions
        .iter()
        .filter(|q| q.profile_id == member || q.profile_id == anchor.profile_id)
        .filter(|q| q.purpose == QuestionPurpose::Anchor && q.state == QuestionState::Open)
        .filter(|q| q.anchor_id.as_deref() == Some(anchor.id.as_str()) && !q.is_overdue(s.now))
        .collect()
}

/// What the model is told when the member taps it: the questions, so the reply can put them.
fn questions_brief(anchor: &Anchor, open: &[&Question]) -> String {
    let mut brief = format!(
        "Goose has {} about {} for the member, so that what it makes for it fits them. Ask \
         them here, one at a time, each with its options, and say they may answer in their \
         own words instead.\n{}",
        plural(open.len(), "open question", "open questions"),
        anchor.title,
        about_anchor_short(anchor)
    );
    for (n, question) in open.iter().take(QUESTIONS_LISTED).enumerate() {
        brief.push_str(&format!(
            "\n{}. {} Options: {}{}.",
            n + 1,
            question.text,
            question.labels().join("; "),
            if question.multi {
                " (more than one may be chosen)"
            } else {
                ""
            }
        ));
    }
    let more = open.len().saturating_sub(QUESTIONS_LISTED);
    if more > 0 {
        brief.push_str(&format!("\n({more} more are in My Goose.)"));
    }
    brief
}

fn plan_to_confirm(s: &SuggestionSnapshot) -> Result<Built, String> {
    let member = caller(s, "a plan Goose noticed")?;
    let offered = s
        .anchors
        .iter()
        .filter(|a| a.profile_id == member && a.state == AnchorState::Candidate)
        // The kinds `plan_for` takes, so the tap can confirm it.
        .filter(|a| {
            matches!(
                a.kind,
                AnchorKind::Event | AnchorKind::Trip | AnchorKind::Purchase | AnchorKind::Project
            )
        })
        .min_by_key(|a| (nearest(a, s.now), std::cmp::Reverse(a.created_at)));
    let Some(anchor) = offered else {
        return Err("Goose has offered to plan nothing they mentioned".to_string());
    };
    let ask = match anchor.kind {
        AnchorKind::Trip => "Plan my {anchor}",
        AnchorKind::Event => "Help me prepare for {anchor}",
        _ => "Keep an eye on {anchor} for me",
    };
    let because = match anchor.why.trim() {
        "" => format!("Goose noticed {} and waits for your yes.", anchor.title),
        why => sentence(why),
    };
    Ok(Built::about(
        Suggestion {
            id: "plan_to_confirm".to_string(),
            prompt: fitted(&anchor.title, |title| ask.replace(ANCHOR_MARK, title)),
            because,
            answered_by: GROUP_SCHEDULE,
            context: Some(SuggestionContext::PersonalMemory {
                source_memory_id: format!("anchor:{}", anchor.id),
                fact: grounding(&format!(
                    "Goose offered to work on this and is waiting for the member's yes:\n{}\n\
                     Asking for it is their yes: call plan_for with kind \"{}\" and title \
                     \"{}\". Its dates and place are already kept.",
                    about_anchor(anchor, &s.timezone),
                    anchor.kind.as_str(),
                    anchor.title
                )),
            }),
        },
        anchor,
        s.now,
    ))
}

/// The member's unopened work on anchors they own: only the owner can mark it seen, so a
/// shared one would be offered for good.
fn unseen_of<'a>(
    s: &'a SuggestionSnapshot,
    member: &'a str,
) -> impl Iterator<Item = &'a UnseenWork> + 'a {
    s.unseen_work.iter().filter(move |w| {
        w.anchor.profile_id == member
            && w.deliverable.anchor_id == w.anchor.id
            && w.deliverable.is_unseen()
    })
}

/// A refresh that changed something: `changed_since` offers it, never `ready_deliverable`.
fn is_change(deliverable: &Deliverable) -> bool {
    deliverable.state == DeliverableState::Ready
        && deliverable.version > 1
        && !deliverable.changed.trim().is_empty()
}

fn changed_since(s: &SuggestionSnapshot) -> Result<Built, String> {
    let member = caller(s, "what Goose made")?;
    let Some(work) = unseen_of(s, member).find(|w| is_change(&w.deliverable)) else {
        return Err("nothing Goose refreshed has changed since it was last read".to_string());
    };
    let (updated, change) = ready_notice(&work.anchor, &work.deliverable);
    let made = &work.deliverable;
    Ok(Built::about(
        Suggestion {
            id: "changed_since".to_string(),
            prompt: fitted(&work.anchor.title, |title| {
                change_question(&made.slot, made.kind, title)
            }),
            because: format!("{updated}. {}", sentence(&change)),
            answered_by: GROUP_MEMORY,
            context: Some(SuggestionContext::PersonalMemory {
                source_memory_id: format!("deliverable:{}", made.id),
                fact: grounding(&what_changed(work)),
            }),
        },
        &work.anchor,
        s.now,
    ))
}

/// How the member asks what a refresh changed, by the slot it fills.
fn change_question(slot: &str, kind: DeliverableKind, title: &str) -> String {
    let asked = match slot {
        "dossier" => "What changed in my dossier for {anchor}?",
        "plan" => "What changed in my plan for {anchor}?",
        "stays" => "What changed in the places to stay for {anchor}?",
        "packing" => "What changed in my packing list for {anchor}?",
        "shortlist" => "What changed in my shortlist for {anchor}?",
        "costs" => "What changed in what {anchor} will cost?",
        "digest" => "What's new in {anchor} since last time?",
        "out" => "What changed in what's on near me this week?",
        "month" => "What changed in my spending this month?",
        _ => return format!("What changed in my {} for {title}?", kind.as_str()),
    };
    asked.replace(ANCHOR_MARK, title)
}

/// What the model is told when the member taps it: what changed, and the new opening.
fn what_changed(work: &UnseenWork) -> String {
    let made = &work.deliverable;
    let mut fact = format!(
        "Goose refreshed \"{}\", which it made for {}. What changed:\n{}",
        made.title,
        work.anchor.title,
        cut(made.changed.trim(), CHANGED_CHARS)
    );
    let body = made.body.trim();
    if !body.is_empty() {
        let opening: String = body.chars().take(WORK_CHARS).collect();
        fact.push_str("\nIt now begins:\n");
        fact.push_str(&opening);
        if opening.len() < body.len() {
            fact.push_str("\n(The rest is in My Goose.)");
        }
    }
    fact
}

fn ready_deliverable(s: &SuggestionSnapshot) -> Result<Built, String> {
    let member = caller(s, "what Goose made")?;
    let mut unseen = unseen_of(s, member).peekable();
    if unseen.peek().is_none() {
        return Err("nothing Goose made is waiting to be read".to_string());
    }
    let Some(work) = unseen.find(|w| !is_change(&w.deliverable)) else {
        return Err("what waits to be read is a refresh, which changed_since offers".to_string());
    };
    let (ready, _) = ready_notice(&work.anchor, &work.deliverable);
    Ok(Built::about(
        Suggestion {
            id: "ready_deliverable".to_string(),
            prompt: fitted(&work.anchor.title, |title| {
                let shown = Anchor {
                    title: title.to_string(),
                    ..work.anchor.clone()
                };
                asked_about(&shown, &work.deliverable)
            }),
            because: format!("{ready}."),
            answered_by: GROUP_MEMORY,
            context: Some(SuggestionContext::PersonalMemory {
                source_memory_id: format!("deliverable:{}", work.deliverable.id),
                fact: what_was_made(work),
            }),
        },
        &work.anchor,
        s.now,
    ))
}

/// What the model is told when the member taps it: the deliverable's title, hook and opening.
fn what_was_made(work: &UnseenWork) -> String {
    let made = &work.deliverable;
    let mut fact = format!(
        "\"{}\", which Goose made for {}, is ready for them to read.",
        made.title, work.anchor.title
    );
    if !made.hook.trim().is_empty() {
        fact.push(' ');
        fact.push_str(made.hook.trim());
    }
    let body = made.body.trim();
    if !body.is_empty() {
        let opening: String = body.chars().take(WORK_CHARS).collect();
        fact.push_str("\nIt begins:\n");
        fact.push_str(&opening);
        if opening.len() < body.len() {
            fact.push_str("\n(The rest is in My Goose.)");
        }
    }
    fact
}

/// One ask a fact line can become: a line of `category` whose tool hints name one of `tools`.
struct FactAsk {
    category: FactCategory,
    tools: &'static [&'static str],
    answered_by: &'static str,
    ask: &'static str,
    /// The ask naming the place the household set, used when it set one.
    at_place: Option<&'static str>,
}

const WEATHER_TOOLS: &[&str] = &[TOOL_WEATHER_FORECAST, TOOL_WEATHER_NOW];

/// In preference order for a line that fits more than one; interests and plans have none, as
/// their anchors already get a digest or a plan.
const FACT_ASKS: &[FactAsk] = &[
    FactAsk {
        category: FactCategory::Home,
        tools: WEATHER_TOOLS,
        answered_by: GROUP_WEATHER,
        ask: "What's the weather at home this weekend?",
        at_place: Some("What's the weather in {place} this weekend?"),
    },
    FactAsk {
        category: FactCategory::Routine,
        tools: WEATHER_TOOLS,
        answered_by: GROUP_WEATHER,
        ask: "Will the weather suit my routine this week?",
        at_place: None,
    },
    FactAsk {
        category: FactCategory::Hunt,
        tools: &[TOOL_WEB_WATCH],
        answered_by: GROUP_KNOWLEDGE,
        ask: "Watch the web for what I'm looking to buy",
        at_place: None,
    },
    FactAsk {
        category: FactCategory::Work,
        tools: &[TOOL_SEARCH_WEB],
        answered_by: GROUP_KNOWLEDGE,
        ask: "What's new in my line of work this week?",
        at_place: None,
    },
    FactAsk {
        category: FactCategory::Food,
        tools: &[TOOL_SEARCH_WEB],
        answered_by: GROUP_KNOWLEDGE,
        ask: "Find me a dinner idea that suits how I eat",
        at_place: None,
    },
];

/// The first ask that fits `line`: its category, and a hinted tool this pond can use now.
fn ask_for(line: &FactLine, ready: &BTreeSet<String>) -> Option<&'static FactAsk> {
    FACT_ASKS.iter().find(|ask| {
        ask.category == line.category
            && ask.tools.iter().any(|tool| {
                ready.contains(*tool) && line.tools.iter().any(|hint| hint.as_str() == *tool)
            })
    })
}

fn fact_hint(s: &SuggestionSnapshot) -> Result<Built, String> {
    let member = caller(s, "what Goose knows about someone")?;
    let mut lines: Vec<FactLine> = s
        .facts
        .iter()
        .filter(|line| line.profile_id == member && line.state == FactState::Active)
        .cloned()
        .collect();
    if lines.is_empty() {
        return Err("Goose has no fact sheet for them yet".to_string());
    }
    rank(&mut lines);
    let fitting: Vec<(FactLine, &FactAsk)> = lines
        .into_iter()
        .filter_map(|line| {
            let ask = ask_for(&line, &s.tools_ready)?;
            Some((line, ask))
        })
        .take(FACT_HINT_TURNS)
        .collect();
    if fitting.is_empty() {
        return Err("no line of their fact sheet names a tool this pond can use now".to_string());
    }
    let day = s
        .now
        .with_timezone(&zone(s))
        .date_naive()
        .num_days_from_ce();
    let (line, ask) = &fitting[day.rem_euclid(fitting.len() as i32) as usize];
    let place = s
        .home_place
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let prompt = match (ask.at_place, place) {
        (Some(at_place), Some(place)) => fitted(place, |p| at_place.replace(PLACE_MARK, p)),
        _ => ask.ask.to_string(),
    };
    Ok(Built::undated(Suggestion {
        id: "fact_hint".to_string(),
        prompt,
        because: format!("On your fact sheet: {}", sentence(&line.fact)),
        answered_by: ask.answered_by,
        context: Some(SuggestionContext::PersonalMemory {
            source_memory_id: format!("fact:{}", line.id),
            fact: grounding(&line.fact),
        }),
    }))
}

fn calendar_today(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if !s.audience.may_see_personal() {
        return Err(
            "a calendar belongs to one person, and more than one member lives here \
                    without saying who is asking"
                .to_string(),
        );
    }
    let Some(count) = s.calendar_events_today else {
        return Err("no calendar account is connected".to_string());
    };
    if count == 0 {
        // Silent, not "nothing on today": a question answered "nothing" wastes the one card.
        return Err("a calendar is connected and today is empty".to_string());
    }
    Ok(Suggestion {
        id: "calendar_today".to_string(),
        prompt: "What's on my calendar today?".to_string(),
        because: format!(
            "{} between now and midnight.",
            plural(count, "event", "events")
        ),
        answered_by: GROUP_CONTEXT,
        context: Some(SuggestionContext::Temporal {
            event_title: "Today's calendar schedule".to_string(),
            timing_summary: format!("{count} events between now and midnight"),
        }),
    })
}

fn upcoming_schedule(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if s.schedules_before_midnight == 0 {
        return Err("nothing is set to run before midnight".to_string());
    }
    // The household's own label, never a description invented here.
    let because = match &s.next_schedule_label {
        Some(label) => format!(
            "{} before midnight; the next is {label}.",
            plural(s.schedules_before_midnight, "routine runs", "routines run")
        ),
        None => format!(
            "{} before midnight.",
            plural(s.schedules_before_midnight, "routine runs", "routines run")
        ),
    };
    Ok(Suggestion {
        id: "upcoming_schedule".to_string(),
        prompt: "What's set to run before tonight?".to_string(),
        because: because.clone(),
        answered_by: GROUP_SCHEDULE,
        context: Some(SuggestionContext::Temporal {
            event_title: "Automated routines".to_string(),
            timing_summary: because,
        }),
    })
}

fn inbox_recent(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if !s.audience.may_see_personal() {
        return Err(
            "a mailbox belongs to one person, and more than one member lives here \
                    without saying who is asking"
                .to_string(),
        );
    }
    let Some(count) = s.mail_items_this_week else {
        return Err("no mail account is connected".to_string());
    };
    if count == 0 {
        return Err("a mail account is connected and nothing arrived this week".to_string());
    }
    Ok(Suggestion {
        id: "inbox_recent".to_string(),
        prompt: "What should I know from my inbox this week?".to_string(),
        because: format!(
            "{} landed in the last seven days.",
            plural(count, "message", "messages")
        ),
        answered_by: GROUP_CONTEXT,
        context: Some(SuggestionContext::Temporal {
            event_title: "Recent mailbox activity".to_string(),
            timing_summary: format!("{count} messages landed in the last seven days"),
        }),
    })
}

fn routine_recall(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if !s.audience.may_see_personal() {
        return Err(
            "what somebody does regularly is about them, and more than one member \
                    lives here without saying who is asking"
                .to_string(),
        );
    }
    if s.routine_memories == 0 {
        return Err("nothing has been remembered as a standing habit yet".to_string());
    }
    Ok(Suggestion {
        id: "routine_recall".to_string(),
        prompt: "What do you know about my routines?".to_string(),
        // Notes filed, not observations: nothing counts repeats, so "you usually" can't be backed.
        because: format!(
            "{} filed as something you do regularly.",
            plural(s.routine_memories, "note", "notes")
        ),
        answered_by: GROUP_MEMORY,
        context: Some(SuggestionContext::PersonalMemory {
            source_memory_id: "routines".to_string(),
            fact: format!(
                "{} routine habits recorded in personal profile",
                s.routine_memories
            ),
        }),
    })
}

fn memory_recall(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if !s.audience.may_see_personal() {
        return Err(
            "what the pond remembers is about a person, and more than one member \
                    lives here without saying who is asking"
                .to_string(),
        );
    }
    if s.active_memories == 0 {
        return Err("nothing has been remembered yet".to_string());
    }
    Ok(Suggestion {
        id: "memory_recall".to_string(),
        prompt: "What do you remember about me?".to_string(),
        because: format!(
            "{} the pond can still reach.",
            plural(s.active_memories, "thing remembered", "things remembered")
        ),
        answered_by: GROUP_MEMORY,
        context: Some(SuggestionContext::PersonalMemory {
            source_memory_id: "active_memories".to_string(),
            fact: format!(
                "{} active memories recorded in personal profile",
                s.active_memories
            ),
        }),
    })
}

fn devices_online(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if s.devices_registered == 0 {
        return Err("no devices are registered with this pond".to_string());
    }
    Ok(Suggestion {
        id: "devices_online".to_string(),
        prompt: "Which of my devices are online?".to_string(),
        // Registered, never "on": `LoggingDeviceControl` reads no state; `is_online` is real.
        because: format!(
            "{} registered here.",
            plural(s.devices_registered, "device", "devices")
        ),
        answered_by: GROUP_DEVICE,
        context: Some(SuggestionContext::Environmental {
            device_ids: Vec::new(),
            summary: format!("{} devices currently registered", s.devices_registered),
        }),
    })
}

fn weather_today(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if !s.weather_ready {
        return Err("weather is off, or this pond has no coordinates".to_string());
    }
    let because = match &s.place {
        Some(place) => format!("Weather is on and this pond is set to {place}."),
        None => "Weather is on and this pond has coordinates.".to_string(),
    };
    Ok(Suggestion {
        id: "weather_today".to_string(),
        prompt: "What's the weather here today?".to_string(),
        because,
        answered_by: GROUP_WEATHER,
        context: Some(SuggestionContext::Temporal {
            event_title: "Local weather forecast".to_string(),
            timing_summary: "Today's forecast".to_string(),
        }),
    })
}

// ── Wording ─────────────────────────────────────────────────────────────────

fn zone(s: &SuggestionSnapshot) -> Tz {
    s.timezone.parse().unwrap_or(chrono_tz::UTC)
}

/// Whole days from `now`'s date to `at`'s, on the household's clock.
fn days_until(now: DateTime<Utc>, at: DateTime<Utc>, tz: Tz) -> i64 {
    (at.with_timezone(&tz).date_naive() - now.with_timezone(&tz).date_naive()).num_days()
}

/// "today", "tomorrow" or "on Thursday", for a day less than a week off.
fn day_words(now: DateTime<Utc>, at: DateTime<Utc>, tz: Tz) -> String {
    match days_until(now, at, tz) {
        0 => "today".to_string(),
        1 => "tomorrow".to_string(),
        _ => format!("on {}", at.with_timezone(&tz).format("%A")),
    }
}

/// Seconds between now and an anchor's date, its start or else its end; `None` for no date.
fn distance(anchor: &Anchor, now: DateTime<Utc>) -> Option<i64> {
    anchor
        .starts_at
        .or(anchor.ends_at)
        .map(|at| (at - now).num_seconds().abs())
}

/// Orders anchors nearest date first and the undated last.
fn nearest(anchor: &Anchor, now: DateTime<Utc>) -> (bool, i64) {
    let away = distance(anchor, now);
    (away.is_none(), away.unwrap_or(0))
}

/// An anchor as the member says it: "my Diani trip" for what they are doing, the bare title of
/// an event or an interest, and "the" before what someone else shared with them.
fn named(kind: AnchorKind, title: &str, mine: bool) -> String {
    match kind {
        AnchorKind::Trip | AnchorKind::Purchase | AnchorKind::Project if mine => {
            format!("my {title}")
        }
        AnchorKind::Trip | AnchorKind::Purchase | AnchorKind::Project => format!("the {title}"),
        _ => title.to_string(),
    }
}

fn kind_words(kind: AnchorKind) -> &'static str {
    match kind {
        AnchorKind::Event => "an event",
        AnchorKind::Trip => "a trip",
        AnchorKind::Purchase => "something to buy",
        AnchorKind::Project => "a project",
        AnchorKind::Interest => "something they follow",
        AnchorKind::Standing => "Goose's standing work for them",
    }
}

/// An anchor's title and kind, for a line of grounding.
fn about_anchor_short(anchor: &Anchor) -> String {
    format!("{} is {}.", anchor.title, kind_words(anchor.kind))
}

/// An anchor for a card's grounding: what it is, when, where, and the facts it holds.
fn about_anchor(anchor: &Anchor, timezone: &str) -> String {
    let mut about = format!("{}: {}", anchor.title, kind_words(anchor.kind));
    if let Some(when) = when_of(anchor, timezone) {
        about.push(' ');
        about.push_str(&when);
    }
    if let Some(place) = anchor
        .place
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        about.push_str(&format!(", at {place}"));
    }
    about.push('.');
    for fact in &anchor.facts {
        about.push_str(&format!("\n{}: {}", fact.label, fact.value));
    }
    about
}

/// Text a card's grounding carries: bounded, and with nothing that could close its envelope.
fn grounding(text: &str) -> String {
    cut(&text.replace('<', "(").replace('>', ")"), GROUNDING_CHARS)
}

/// `text` ending in a full stop, unless it already ends a sentence.
fn sentence(text: &str) -> String {
    let text = text.trim();
    if text.ends_with(['.', '!', '?']) {
        text.to_string()
    } else {
        format!("{text}.")
    }
}

/// `text` within `max` characters, cut at a word and marked "..." when it had to be cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(3)).collect();
    let kept = match kept.rfind(char::is_whitespace) {
        Some(at) if at > 0 => kept[..at].trim_end(),
        _ => kept.as_str(),
    };
    format!("{kept}...")
}

/// `ask` with `title` in it, the title cut at a word so the whole fits [`MAX_PROMPT_CHARS`].
fn fitted(title: &str, ask: impl Fn(&str) -> String) -> String {
    let whole = ask(title);
    let over = whole.chars().count().saturating_sub(MAX_PROMPT_CHARS);
    if over == 0 {
        return whole;
    }
    ask(&cut(title, title.chars().count().saturating_sub(over)))
}

/// "1 event" / "3 events"; the caller spells both ("routine runs" / "routines run").
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

#[cfg(test)]
mod tests;
