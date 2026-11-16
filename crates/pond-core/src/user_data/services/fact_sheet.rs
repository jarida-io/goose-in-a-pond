//! The fact sheet's maker and critic: their prompts, the reading of their replies, when a
//! rebuild is due and how one is asked for, and the sheet as briefs and older readers take it.
//! Nothing here calls a model.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;

use crate::user_data::domain::anchor::{Anchor, AnchorKind, AnchorState};
use crate::user_data::domain::dont_list::DontRule;
use crate::user_data::domain::engine_job::{EngineJob, JobClass, JobKind, JobState};
use crate::user_data::domain::fact_sheet::{
    fact_key, score, use_weight, Confidence, FactCategory, FactLine, FactSource, FactSourceKind,
    FactState, FACT_SHEET_LIMIT,
};
use crate::user_data::domain::interest::{Interest, MemberPortrait};
use crate::user_data::domain::member_profile::describes_a_question;
use crate::user_data::domain::memory::{MemoryFragment, MemorySegment};
use crate::user_data::domain::question::{Question, QuestionState};
use crate::user_data::ports::engine_jobs::EngineJobRepository;
use crate::user_data::services::model_json::{list_in, text_of, texts_of};

/// A rebuild is due this long after the last one finished.
pub const REBUILD_EVERY: Duration = Duration::hours(24);
/// New memories since the last rebuild that make one due before the day is out.
pub const REBUILD_AFTER_MEMORIES: usize = 5;

/// Sources of each kind one rebuild shows the maker; together with a full sheet and a full
/// reply they stay inside a 32k window.
pub const MAX_MEMORIES: usize = 80;
pub const MAX_SUMMARIES: usize = 8;
pub const MAX_INTERESTS: usize = 20;
pub const MAX_ANCHORS: usize = 12;
pub const MAX_ANSWERS: usize = 30;
const MAX_SOURCE_CHARS: usize = 320;
const MAX_SUMMARY_CHARS: usize = 600;
const MAX_REMOVED_SHOWN: usize = 40;
const MAX_SINCE_CHARS: usize = 60;
const MAX_REASON_CHARS: usize = 160;
const PORTRAIT_SUMMARY_LINES: usize = 4;
const PORTRAIT_ITEMS: usize = 10;
const ABOUT_INTERESTS: usize = 12;

/// Whether a member's sheet is due a rebuild: never built, a day old, or five memories behind.
pub fn is_due(
    last_finished: Option<DateTime<Utc>>,
    new_memories_since: usize,
    now: DateTime<Utc>,
) -> bool {
    match last_finished {
        None => true,
        Some(at) => now - at >= REBUILD_EVERY || new_memories_since >= REBUILD_AFTER_MEMORIES,
    }
}

/// Asks for a rebuild of a member's sheet the way every engine job is asked for: a queued
/// job with no task, which the engine's runner picks up. A member with a rebuild already open
/// gets that one, moved up to `class` while it still waits.
pub async fn request_rebuild(
    jobs: &dyn EngineJobRepository,
    profile_id: &str,
    class: JobClass,
    now: DateTime<Utc>,
) -> anyhow::Result<EngineJob> {
    let open = jobs.open_of_kind(profile_id, JobKind::FactSheet).await?;
    let Some(mut job) = open.into_iter().next() else {
        let job = EngineJob::new(
            JobKind::FactSheet,
            class,
            Some(profile_id),
            "Fact sheet",
            now,
        );
        jobs.add(&job).await?;
        return Ok(job);
    };
    if job.state == JobState::Queued && class < job.class {
        job.class = class;
        jobs.save(&job).await?;
    }
    Ok(job)
}

/// One numbered thing the maker may rest a line on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub kind: FactSourceKind,
    /// The memory, conversation, interest, anchor or question it is.
    pub reference: String,
    pub text: String,
    /// When it was said or last changed: a line is as recent as its newest source.
    pub at: DateTime<Utc>,
}

/// One conversation's rolling summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSummary {
    pub session_id: String,
    pub title: String,
    pub summary: String,
    pub updated_at: DateTime<Utc>,
}

/// A tool this pond has, as the maker is told of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolHint {
    pub name: String,
    /// What it is for, in one line.
    pub about: String,
}

impl ToolHint {
    pub fn new(name: &str, about: &str) -> Self {
        Self {
            name: name.trim().to_string(),
            about: one_line(about, MAX_SOURCE_CHARS),
        }
    }
}

fn one_line(text: &str, max_chars: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

/// The memories worth reading first: most important, newer first within a level.
pub fn top_memories(mut memories: Vec<MemoryFragment>, limit: usize) -> Vec<MemoryFragment> {
    memories.sort_by(|a, b| {
        let importance = |m: &MemoryFragment| (m.importance.unwrap_or(0.5) * 10.0).round() as i32;
        importance(b)
            .cmp(&importance(a))
            .then(b.created_at.cmp(&a.created_at))
    });
    memories.truncate(limit);
    memories
}

fn interest_source(interest: &Interest) -> String {
    if interest.avoid {
        return format!("Does not want to hear about {}", interest.topic);
    }
    let mut text = format!("Follows {} ({})", interest.topic, interest.depth.as_str());
    if !interest.facets.is_empty() {
        text.push_str(&format!(": {}", interest.facets.join(", ")));
    }
    text
}

fn anchor_source(anchor: &Anchor, tz: Tz) -> String {
    let kind = match anchor.kind {
        AnchorKind::Event => "An event",
        AnchorKind::Trip => "A trip",
        AnchorKind::Purchase => "Something to buy",
        AnchorKind::Project => "A project",
        AnchorKind::Interest | AnchorKind::Standing => "Something they follow",
    };
    let day = |at: DateTime<Utc>| at.with_timezone(&tz).format("%-d %B %Y").to_string();
    let mut text = format!("{kind}: {}", anchor.title);
    match (anchor.starts_at, anchor.ends_at) {
        (Some(from), Some(to)) if day(from) != day(to) => {
            text.push_str(&format!(", {} to {}", day(from), day(to)));
        }
        (Some(at), _) | (None, Some(at)) => text.push_str(&format!(", {}", day(at))),
        (None, None) => {}
    }
    if let Some(place) = anchor.place.as_deref().filter(|p| !p.trim().is_empty()) {
        text.push_str(&format!(", at {}", place.trim()));
    }
    for fact in &anchor.facts {
        text.push_str(&format!("; {}: {}", fact.label, fact.value));
    }
    text
}

/// What a rebuild reads about one member, before it is cut down and numbered.
#[derive(Debug, Clone, Default)]
pub struct SourceRows<'a> {
    pub memories: Vec<MemoryFragment>,
    /// The member's conversations, newest first.
    pub summaries: &'a [ConversationSummary],
    pub interests: &'a [Interest],
    pub anchors: &'a [Anchor],
    pub answers: &'a [Question],
}

/// The maker's sources in the order they are numbered: the member's memories (most important
/// first), conversation summaries, confirmed interests, active anchors and answers. A row of
/// another member, or of nobody, is never a source.
pub fn sources(profile_id: &str, rows: SourceRows<'_>, tz: Tz, now: DateTime<Utc>) -> Vec<Source> {
    let memories: Vec<MemoryFragment> = rows
        .memories
        .into_iter()
        .filter(|m| m.profile_id.as_deref() == Some(profile_id) && m.is_shown(now))
        .filter(|m| m.segment != Some(MemorySegment::Knowledge))
        .filter(|m| !describes_a_question(&m.content))
        .collect();
    let mut out: Vec<Source> = top_memories(memories, MAX_MEMORIES)
        .into_iter()
        .map(|m| Source {
            kind: FactSourceKind::Memory,
            text: one_line(&m.content, MAX_SOURCE_CHARS),
            reference: m.id,
            at: m.created_at,
        })
        .collect();
    out.extend(rows.summaries.iter().take(MAX_SUMMARIES).map(|s| {
        let title = s.title.trim();
        let text = if title.is_empty() {
            s.summary.clone()
        } else {
            format!("{title}: {}", s.summary)
        };
        Source {
            kind: FactSourceKind::Summary,
            reference: s.session_id.clone(),
            text: one_line(&text, MAX_SUMMARY_CHARS),
            at: s.updated_at,
        }
    }));
    out.extend(
        rows.interests
            .iter()
            .filter(|i| i.profile_id == profile_id && i.confirmed)
            .take(MAX_INTERESTS)
            .map(|i| Source {
                kind: FactSourceKind::Interest,
                reference: i.id.clone(),
                text: one_line(&interest_source(i), MAX_SOURCE_CHARS),
                at: i.updated_at,
            }),
    );
    out.extend(
        rows.anchors
            .iter()
            .filter(|a| a.is_readable_by(profile_id) && a.state == AnchorState::Active)
            // The everyday anchor is the pond's own, not something the member is doing.
            .filter(|a| a.kind != AnchorKind::Standing)
            .take(MAX_ANCHORS)
            .map(|a| Source {
                kind: FactSourceKind::Anchor,
                reference: a.id.clone(),
                text: one_line(&anchor_source(a, tz), MAX_SOURCE_CHARS),
                at: a.updated_at,
            }),
    );
    out.extend(
        rows.answers
            .iter()
            .filter(|q| q.profile_id == profile_id && q.state == QuestionState::Answered)
            .filter_map(|q| Some((q, q.answer.as_ref()?)))
            .take(MAX_ANSWERS)
            .map(|(q, answer)| Source {
                kind: FactSourceKind::Answer,
                reference: q.id.clone(),
                text: one_line(
                    &format!("Asked \"{}\", they answered: {}", q.text, answer.as_words()),
                    MAX_SOURCE_CHARS,
                ),
                at: q.answered_at.unwrap_or(q.created_at),
            }),
    );
    out
}

/// The maker's system prompt, fixed so it stays in the model's cached prefix. It has no worked
/// example: a model this size writes the example back.
pub const SYSTEM: &str = "\
You keep the fact sheet about one member of this household: short lines that say who they \
are, what they are doing and what they like. The pond reads the sheet before it makes \
anything for them, so every line must be true to the sources and worth knowing.

You are given numbered sources (their memories, summaries of their conversations, interests \
they confirmed, plans they are following and answers they gave), the current sheet, the \
tools this pond has, and the rules they set. Write the whole sheet again from the sources. \
Keep a line of the current sheet only while a source still supports it, reword it when the \
sources say more, and add what the sources show that the sheet lacks.

Reply with JSON and nothing else:

{\"lines\": [{\"category\": \"<category>\", \"fact\": \"<fact>\", \"confidence\": \
\"<confidence>\", \"since\": \"<when>\", \"sources\": [<numbers>], \"tools\": [\"<tool name>\"]}]}

- At most 100 lines. A few true, specific lines beat many thin ones.
- \"category\": one of identity (who they are), home (where and with whom they live), work \
(their job, studies or business), people (family, friends and colleagues, by name), plan \
(something dated they are preparing for), hunt (something they are looking to buy or find), \
interest (what they follow or do for fun), food (what they eat and drink, and what they \
cannot), health, style (their tastes and how they like things done), routine (what they do \
regularly), avoid (what they want kept away from them), other.
- \"fact\": one plain statement about them, under 160 characters, in the third person and \
without their name. One fact to a line. Use the names, places, amounts and dates the sources \
give, never a general word in their place.
- \"confidence\": \"sure\" when an answer or a confirmed interest says it, or two or more \
sources agree; \"likely\" when you worked it out from what the sources show; \"said\" when \
one source mentions it once.
- \"since\": when it began, in a few words, when a source says so; otherwise \"\".
- \"sources\": the numbers of the sources the line rests on. A line that names no source is \
thrown away.
- \"tools\": names from the tools list that the fact would change the use of, as a home town \
changes a weather lookup. Only names from the list. [] when none.

Never write a line that:
- only says what they asked or looked up once: one question is curiosity, not a fact about \
them;
- is about the pond, the assistant, or how either works;
- says the same thing as another line;
- says something the sources do not;
- repeats a line they removed, or breaks one of their rules.

Lines marked as theirs in the current sheet stay as they are whatever you write: do not \
repeat or contradict them.
No explanation, no preamble.";

fn source_word(kind: FactSourceKind) -> &'static str {
    match kind {
        FactSourceKind::Memory => "memory",
        FactSourceKind::Summary => "conversation",
        FactSourceKind::Interest => "interest",
        FactSourceKind::Anchor => "plan",
        FactSourceKind::Answer => "answer",
        FactSourceKind::Interview => "interview",
        FactSourceKind::You => "theirs",
    }
}

fn hints(tools: &[String]) -> String {
    if tools.is_empty() {
        String::new()
    } else {
        format!(" -> {}", tools.join(", "))
    }
}

/// The maker's message: the numbered sources, the sheet as it stands, the lines the member
/// removed, the pond's tools and the `never` rules. `sheet` is the member's lines in any
/// state; `sources` must be the slice [`parse`] is later given.
pub fn build_message(
    sources: &[Source],
    sheet: &[FactLine],
    tools: &[ToolHint],
    never: &[DontRule],
) -> String {
    let mut out = String::from("Sources:\n");
    if sources.is_empty() {
        out.push_str("None yet.\n");
    }
    for (i, source) in sources.iter().enumerate() {
        out.push_str(&format!(
            "{}. ({}) {}\n",
            i + 1,
            source_word(source.kind),
            source.text
        ));
    }

    out.push_str("\nCurrent sheet:\n");
    let mut held = 0;
    for line in sheet.iter().filter(|l| l.state == FactState::Active) {
        held += 1;
        let theirs = if line.is_protected() { " (theirs)" } else { "" };
        out.push_str(&format!(
            "- [{}] {}{theirs}{}\n",
            line.category.as_str(),
            line.fact,
            hints(&line.tools)
        ));
    }
    if held == 0 {
        out.push_str("Empty.\n");
    }

    let removed: Vec<&FactLine> = sheet
        .iter()
        .filter(|l| l.state == FactState::Removed)
        .take(MAX_REMOVED_SHOWN)
        .collect();
    if !removed.is_empty() {
        out.push_str("\nLines they removed (never write these again):\n");
        for line in removed {
            out.push_str(&format!("- {}\n", line.fact));
        }
    }

    out.push_str("\nTools:\n");
    if tools.is_empty() {
        out.push_str("None.\n");
    }
    for tool in tools {
        out.push_str(&format!("- {}: {}\n", tool.name, tool.about));
    }

    if !never.is_empty() {
        out.push_str("\nTheir rules:\n");
        for rule in never {
            out.push_str(&format!("- {}\n", rule.as_line()));
        }
    }
    out.push_str("\nJSON:");
    out
}

/// Why a line of the maker's reply was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// It cited no source number from the prompt.
    NoSource,
    /// It had no fact, or only the schema's placeholder.
    NoFact,
    /// An earlier line of the reply says the same thing.
    Repeat,
    /// The reply held more than [`FACT_SHEET_LIMIT`] usable lines and this was among the weakest.
    OverLimit,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::NoSource => "names no source from the prompt",
            Refusal::NoFact => "has no fact",
            Refusal::Repeat => "repeats an earlier line",
            Refusal::OverLimit => "past the sheet's limit",
        }
    }
}

/// A line the maker wrote, with the sources it cited.
#[derive(Debug, Clone, PartialEq)]
pub struct Drafted {
    pub line: FactLine,
    /// Positions in the `sources` slice, first cited first.
    pub cites: Vec<usize>,
}

/// What one maker reply came to.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    pub drafted: Vec<Drafted>,
    /// One entry per refused line.
    pub refused: Vec<Refusal>,
    /// Tool names taken off lines because this pond has no such tool.
    pub tools_removed: usize,
    /// The reply held no list of lines at all; distinct from a maker that wrote none.
    pub unparseable: bool,
}

impl Outcome {
    /// The drafted lines, as the store's `replace` takes them.
    pub fn lines(&self) -> Vec<FactLine> {
        self.drafted.iter().map(|d| d.line.clone()).collect()
    }
}

/// The source positions an item cites: numbers from the prompt, each once, in the order given.
fn cited(map: &serde_json::Map<String, serde_json::Value>, source_count: usize) -> Vec<usize> {
    let number = |v: &serde_json::Value| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (1..=source_count).contains(n))
            .map(|n| n - 1)
    };
    let given = map.get("sources").or_else(|| map.get("source"));
    let mut cites: Vec<usize> = Vec::new();
    let all: Vec<&serde_json::Value> = match given {
        Some(serde_json::Value::Array(items)) => items.iter().collect(),
        Some(one) => vec![one],
        None => Vec::new(),
    };
    for position in all.into_iter().filter_map(number) {
        if !cites.contains(&position) {
            cites.push(position);
        }
    }
    cites
}

/// Reads the maker's reply into lines. `sources` must be the slice [`build_message`] numbered.
/// Enforced here, not left to the model: a line citing no source that exists is refused, a
/// tool this pond lacks is taken off its line, a line's source is the first it validly cites,
/// and at most [`FACT_SHEET_LIMIT`] lines come back, the best by score.
pub fn parse(
    raw: &str,
    sources: &[Source],
    known_tools: &[ToolHint],
    profile_id: &str,
    now: DateTime<Utc>,
) -> Outcome {
    let Some(items) = list_in(raw, "lines") else {
        return Outcome {
            unparseable: true,
            ..Default::default()
        };
    };
    let mut outcome = Outcome::default();
    let mut seen = HashSet::new();
    for item in &items {
        let Some(map) = item.as_object() else {
            outcome.refused.push(Refusal::NoFact);
            continue;
        };
        let cites = cited(map, sources.len());
        let Some(first) = cites.first().map(|i| &sources[*i]) else {
            outcome.refused.push(Refusal::NoSource);
            continue;
        };
        let confidence = text_of(map, "confidence")
            .and_then(|c| Confidence::parse(&c))
            .unwrap_or_default();
        let category = text_of(map, "category")
            .map(|c| FactCategory::parse(&c))
            .unwrap_or_default();
        let Some(mut line) = text_of(map, "fact").and_then(|fact| {
            FactLine::new(
                profile_id,
                category,
                &fact,
                confidence,
                FactSource::new(first.kind, Some(&first.reference)),
                now,
            )
        }) else {
            outcome.refused.push(Refusal::NoFact);
            continue;
        };
        if !seen.insert(line.key()) {
            outcome.refused.push(Refusal::Repeat);
            continue;
        }
        line.since = text_of(map, "since").map(|s| one_line(&s, MAX_SINCE_CHARS));
        let named = texts_of(map, "tools");
        for name in &named {
            let known = known_tools
                .iter()
                .find(|t| t.name.eq_ignore_ascii_case(name.trim()));
            match known {
                Some(tool) if !line.tools.contains(&tool.name) => {
                    line.tools.push(tool.name.clone())
                }
                Some(_) => {}
                None => outcome.tools_removed += 1,
            }
        }
        let newest = cites.iter().map(|i| sources[*i].at).max().unwrap_or(now);
        line.score = score(confidence, cites.len() as u32, newest, 0, now);
        outcome.drafted.push(Drafted { line, cites });
    }
    if outcome.drafted.len() > FACT_SHEET_LIMIT {
        outcome
            .drafted
            .sort_by(|a, b| b.line.score.total_cmp(&a.line.score));
        let over = outcome.drafted.len() - FACT_SHEET_LIMIT;
        outcome.drafted.truncate(FACT_SHEET_LIMIT);
        outcome
            .refused
            .extend(std::iter::repeat_n(Refusal::OverLimit, over));
    }
    outcome
}

/// `lines` with the use each has already had on `sheet` counted into its score, so a rebuild
/// does not forget which lines rode turns and briefs.
pub fn counting_use(mut lines: Vec<FactLine>, sheet: &[FactLine]) -> Vec<FactLine> {
    for line in lines.iter_mut() {
        let key = fact_key(&line.fact);
        if let Some(held) = sheet.iter().find(|row| row.key() == key) {
            line.score += use_weight(held.used);
        }
    }
    lines
}

/// The critic's system prompt, fixed like the maker's.
pub const CRITIC_SYSTEM: &str = "\
You check a draft of the fact sheet the pond keeps about one member of this household, \
before it is saved. Be hard to satisfy: a wrong line misleads everything the pond makes for \
them, and a missing line costs little.

You are given the draft's numbered lines, each with the sources it rests on. Name every line \
to drop. Drop a line that:
- only restates something they asked or looked up once;
- is about the pond, the assistant, or how either works, and not about them;
- says the same thing as an earlier line: drop the later one;
- its sources do not support: they say something else, say less, or are about someone else.

Judge each line only by the sources shown under it. Do not rewrite a line and do not add one.

Reply with JSON and nothing else:

{\"drop\": [{\"line\": <number>, \"why\": \"<reason>\"}]}

- \"line\": the number of a draft line.
- \"why\": one short reason, under 100 characters.
- {\"drop\": []} when every line holds.
No explanation, no preamble.";

/// The critic's message: each drafted line under its number, with the text of every source it
/// cites. `sources` must be the slice the draft was parsed against.
pub fn build_critic_message(lines: &[Drafted], sources: &[Source]) -> String {
    let mut out = String::from("Draft:\n");
    if lines.is_empty() {
        out.push_str("No lines.\n");
    }
    for (i, drafted) in lines.iter().enumerate() {
        out.push_str(&format!(
            "{}. [{}] {} ({})\n",
            i + 1,
            drafted.line.category.as_str(),
            drafted.line.fact,
            drafted.line.confidence.as_str()
        ));
        for source in drafted.cites.iter().filter_map(|c| sources.get(*c)) {
            out.push_str(&format!(
                "   - {}: {}\n",
                source_word(source.kind),
                source.text
            ));
        }
    }
    out.push_str("\nJSON:");
    out
}

/// The lines the critic drops, as positions in the draft (from 0) with its reason, lowest
/// first. `None` when the reply cannot be read: the caller discards the whole draft.
pub fn parse_drops(raw: &str, line_count: usize) -> Option<Vec<(usize, String)>> {
    let items = list_in(raw, "drop")?;
    let mut drops: Vec<(usize, String)> = Vec::new();
    for item in &items {
        let (number, why) = match item {
            serde_json::Value::Object(map) => (
                map.get("line").or_else(|| map.get("number")),
                text_of(map, "why").or_else(|| text_of(map, "reason")),
            ),
            other => (Some(other), None),
        };
        let position = number
            .and_then(|n| {
                n.as_u64()
                    .or_else(|| n.as_str().and_then(|s| s.trim().parse().ok()))
            })
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (1..=line_count).contains(n))
            .map(|n| n - 1);
        let Some(position) = position else {
            continue;
        };
        if drops.iter().all(|(held, _)| *held != position) {
            let why = why.map_or_else(String::new, |w| one_line(&w, MAX_REASON_CHARS));
            drops.push((position, why));
        }
    }
    drops.sort_by_key(|(position, _)| *position);
    Some(drops)
}

/// The draft without the lines the critic dropped, as the store's `replace` takes it.
pub fn surviving(drafted: Vec<Drafted>, drops: &[(usize, String)]) -> Vec<FactLine> {
    drafted
        .into_iter()
        .enumerate()
        .filter(|(i, _)| drops.iter().all(|(dropped, _)| dropped != i))
        .map(|(_, d)| d.line)
        .collect()
}

const CATEGORY_ORDER: [(FactCategory, &str); 13] = [
    (FactCategory::Identity, "Who they are"),
    (FactCategory::Home, "Home"),
    (FactCategory::Work, "Work"),
    (FactCategory::People, "People"),
    (FactCategory::Plan, "Plan"),
    (FactCategory::Hunt, "Looking for"),
    (FactCategory::Interest, "Interest"),
    (FactCategory::Food, "Food"),
    (FactCategory::Health, "Health"),
    (FactCategory::Style, "Taste"),
    (FactCategory::Routine, "Routine"),
    (FactCategory::Avoid, "Keep away"),
    (FactCategory::Other, "Other"),
];

/// A category in plain words, as a line of the sheet is led with.
pub fn category_label(category: FactCategory) -> &'static str {
    CATEGORY_ORDER
        .iter()
        .find(|(held, _)| *held == category)
        .map_or("Other", |(_, label)| label)
}

/// The whole active sheet as makers and critics read it: a fact to a line, grouped by
/// category, each line whole on its own with the tools it bears on. Empty when the sheet is.
pub fn sheet_for_brief(lines: &[FactLine]) -> String {
    let mut out: Vec<String> = Vec::new();
    for (category, label) in CATEGORY_ORDER {
        for line in lines
            .iter()
            .filter(|l| l.state == FactState::Active && l.category == category)
        {
            let mut notes: Vec<String> = Vec::new();
            if let Some(since) = line.since.as_deref().filter(|s| !s.trim().is_empty()) {
                notes.push(format!("since {}", since.trim()));
            }
            match line.confidence {
                Confidence::Sure => {}
                Confidence::Likely => notes.push("likely".to_string()),
                Confidence::Said => notes.push("said once".to_string()),
            }
            let notes = if notes.is_empty() {
                String::new()
            } else {
                format!(" ({})", notes.join("; "))
            };
            out.push(format!(
                "{label}: {}{notes}{}",
                line.fact,
                hints(&line.tools)
            ));
        }
    }
    out.join("\n")
}

fn as_sentence(fact: &str) -> String {
    let fact = fact.trim();
    if fact.ends_with(['.', '!', '?']) {
        fact.to_string()
    } else {
        format!("{fact}.")
    }
}

/// The portrait older readers take, from the active lines in the order given; `None` for a
/// sheet with none.
pub fn as_portrait(lines: &[FactLine]) -> Option<MemberPortrait> {
    let active: Vec<&FactLine> = lines
        .iter()
        .filter(|l| l.state == FactState::Active)
        .collect();
    let updated_at = active.iter().map(|l| l.updated_at).max()?;
    let facts_of = |categories: &[FactCategory], limit: usize| -> Vec<String> {
        active
            .iter()
            .filter(|l| categories.contains(&l.category))
            .take(limit)
            .map(|l| l.fact.clone())
            .collect()
    };
    let summary = facts_of(
        &[
            FactCategory::Identity,
            FactCategory::Work,
            FactCategory::Home,
        ],
        PORTRAIT_SUMMARY_LINES,
    )
    .iter()
    .map(|fact| as_sentence(fact))
    .collect::<Vec<_>>()
    .join(" ");
    Some(MemberPortrait {
        summary,
        into: facts_of(&[FactCategory::Interest], PORTRAIT_ITEMS),
        working_on: facts_of(&[FactCategory::Plan, FactCategory::Hunt], PORTRAIT_ITEMS),
        likes: facts_of(&[FactCategory::Style, FactCategory::Food], PORTRAIT_ITEMS),
        updated_at,
    })
}

/// What a prompt is told about a member: the sheet as a portrait, the interests they
/// confirmed and what they asked to be kept away from. `None` when nothing is known yet.
pub fn about_from_sheet(lines: &[FactLine], interests: &[Interest]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(portrait) = as_portrait(lines) {
        parts.push(portrait.as_prompt_lines());
    }
    let followed: Vec<String> = interests
        .iter()
        .filter(|i| i.feeds_research())
        .take(ABOUT_INTERESTS)
        .map(|i| {
            if i.facets.is_empty() {
                i.topic.clone()
            } else {
                format!("{} ({})", i.topic, i.facets.join(", "))
            }
        })
        .collect();
    if !followed.is_empty() {
        parts.push(format!("Confirmed interests: {}", followed.join("; ")));
    }
    let avoided: Vec<&str> = interests
        .iter()
        .filter(|i| i.avoid)
        .map(|i| i.topic.as_str())
        .collect();
    if !avoided.is_empty() {
        parts.push(format!("Never raise: {}", avoided.join("; ")));
    }
    parts.retain(|part| !part.trim().is_empty());
    (!parts.is_empty()).then(|| parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::anchor::{AnchorFact, AnchorOrigin};
    use crate::user_data::domain::dont_list::{RuleOrigin, Strength};
    use crate::user_data::domain::interest::{Depth, NewsMode, Origin};
    use crate::user_data::domain::question::{Answer, QuestionPurpose};
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn source(kind: FactSourceKind, reference: &str, text: &str, days_old: i64) -> Source {
        Source {
            kind,
            reference: reference.into(),
            text: text.into(),
            at: now() - Duration::days(days_old),
        }
    }

    fn three_sources() -> Vec<Source> {
        vec![
            source(
                FactSourceKind::Memory,
                "m1",
                "Runs Jarida, which builds an offline home assistant.",
                40,
            ),
            source(
                FactSourceKind::Answer,
                "q1",
                "Asked \"Where do you live?\", they answered: Kilimani",
                2,
            ),
            source(
                FactSourceKind::Interest,
                "i1",
                "Follows Formula 1 (keen): Ferrari",
                9,
            ),
        ]
    }

    fn tools() -> Vec<ToolHint> {
        vec![
            ToolHint::new("get_weather", "The forecast for a place"),
            ToolHint::new("search_web", "Look something up on the web"),
        ]
    }

    fn held(category: FactCategory, fact: &str, confidence: Confidence) -> FactLine {
        FactLine::new(
            "jerry",
            category,
            fact,
            confidence,
            FactSource::new(FactSourceKind::Memory, Some("m1")),
            now(),
        )
        .unwrap()
    }

    fn memory(id: &str, content: &str, importance: f32, days_old: i64) -> MemoryFragment {
        let mut m = MemoryFragment::from_extraction(
            id.to_string(),
            None,
            content.to_string(),
            MemorySegment::Identity,
            importance,
            None,
        );
        m.profile_id = Some("jerry".into());
        m.created_at = now() - Duration::days(days_old);
        m
    }

    fn interest(topic: &str, confirmed: bool, avoid: bool) -> Interest {
        Interest {
            id: topic.to_lowercase(),
            profile_id: "jerry".into(),
            topic: topic.into(),
            category: "other".into(),
            facets: vec![],
            depth: Depth::Keen,
            news: NewsMode::Both,
            avoid,
            origin: Origin::Chat,
            evidence: 5,
            confirmed,
            picture: None,
            updated_at: now(),
        }
    }

    #[test]
    fn a_sheet_is_due_daily_or_five_memories_on() {
        assert!(is_due(None, 0, now()), "never built");
        let an_hour_ago = Some(now() - Duration::hours(1));
        assert!(!is_due(an_hour_ago, 4, now()));
        assert!(is_due(an_hour_ago, 5, now()));
        assert!(!is_due(Some(now() - Duration::hours(23)), 0, now()));
        assert!(is_due(Some(now() - Duration::hours(24)), 0, now()));
    }

    #[tokio::test]
    async fn a_rebuild_is_asked_for_once_and_an_ask_by_the_member_moves_it_up() {
        use crate::user_data::mocks::mock_engine_jobs::InMemoryEngineJobRepository;
        let jobs = InMemoryEngineJobRepository::new();
        let first = request_rebuild(&jobs, "jerry", JobClass::Upkeep, now())
            .await
            .unwrap();
        assert_eq!(first.kind, JobKind::FactSheet);
        assert_eq!(first.state, JobState::Queued);
        assert_eq!(first.task_id, None);
        assert_eq!(first.profile_id.as_deref(), Some("jerry"));

        let asked = request_rebuild(&jobs, "jerry", JobClass::Asked, now())
            .await
            .unwrap();
        assert_eq!(asked.id, first.id, "one open rebuild a member");
        assert_eq!(asked.class, JobClass::Asked);
        let again = request_rebuild(&jobs, "jerry", JobClass::Upkeep, now())
            .await
            .unwrap();
        assert_eq!(again.class, JobClass::Asked, "never moved back down");

        let hers = request_rebuild(&jobs, "liz", JobClass::Upkeep, now())
            .await
            .unwrap();
        assert_ne!(hers.id, first.id);

        let mut done = again;
        done.start(None, now()).unwrap();
        done.settle(JobState::Passed, "kept", now()).unwrap();
        jobs.save(&done).await.unwrap();
        let next = request_rebuild(&jobs, "jerry", JobClass::Upkeep, now())
            .await
            .unwrap();
        assert_ne!(
            next.id, first.id,
            "a settled rebuild does not stand in the way"
        );
        assert_eq!(jobs.open().await.unwrap().len(), 2);
    }

    #[test]
    fn the_prompts_state_their_caps_and_shape_and_hold_no_example_to_echo() {
        for field in [
            "\"category\"",
            "\"fact\"",
            "\"confidence\"",
            "\"since\"",
            "\"sources\"",
            "\"tools\"",
        ] {
            assert!(
                SYSTEM.matches(field).count() >= 2,
                "{field} is asked for and never defined"
            );
        }
        assert!(SYSTEM.contains("At most 100 lines"));
        assert!(SYSTEM.contains("Reply with JSON and nothing else"));
        assert!(SYSTEM.contains("one question is curiosity"));
        assert!(CRITIC_SYSTEM.contains("{\"drop\": []}"));
        for category in ["identity", "hunt", "routine", "avoid"] {
            assert!(SYSTEM.contains(category), "{category} is never offered");
        }
        // Every placeholder is one `text_of` refuses, so an echoed shape yields no line.
        let echoed = parse(
            "{\"lines\": [{\"category\": \"<category>\", \"fact\": \"<fact>\", \"sources\": [1]}]}",
            &three_sources(),
            &tools(),
            "jerry",
            now(),
        );
        assert!(echoed.drafted.is_empty());
        assert_eq!(echoed.refused, [Refusal::NoFact]);
    }

    #[test]
    fn the_sources_are_numbered_in_a_fixed_order_and_leave_out_what_proves_nothing() {
        let memories = vec![
            memory("old", "Lives in Kilimani.", 0.6, 30),
            memory("weighty", "Is allergic to peanuts.", 0.9, 90),
            memory("asked", "Jerry asked about the weather in Kisumu.", 0.9, 1),
        ];
        let mut trivia = memory("trivia", "The Nile is 6,650 km long.", 0.9, 1);
        trivia.segment = Some(MemorySegment::Knowledge);
        let mut memories = memories;
        memories.push(trivia);

        let mut trip = Anchor::new(
            "jerry",
            AnchorKind::Trip,
            "Mombasa with Liz",
            AnchorOrigin::You,
            now(),
        )
        .unwrap();
        trip.starts_at = Some(Utc.with_ymd_and_hms(2026, 12, 19, 21, 30, 0).unwrap());
        trip.ends_at = Some(Utc.with_ymd_and_hms(2026, 12, 23, 9, 0, 0).unwrap());
        trip.place = Some("Diani".into());
        trip.set_fact(AnchorFact::new("Budget", "KSh 80,000", "you"), now());
        let candidate = Anchor::new(
            "jerry",
            AnchorKind::Event,
            "A wedding",
            AnchorOrigin::Chat,
            now(),
        )
        .unwrap();
        let everyday = Anchor::new(
            "jerry",
            AnchorKind::Standing,
            "Everyday",
            AnchorOrigin::Goose,
            now(),
        )
        .unwrap();

        let mut asked = Question::new(
            "jerry",
            "b1",
            QuestionPurpose::Profile,
            "How do you get to work?",
            &["Drive".into(), "Matatu".into()],
            false,
            now(),
        )
        .unwrap();
        let open = asked.clone();
        asked
            .answer(
                Answer {
                    choices: vec!["Drive".into()],
                    text: Some("a Fielder".into()),
                },
                now(),
            )
            .unwrap();

        let mut lizs_memory = memory("hers", "Is training for a marathon.", 0.9, 1);
        lizs_memory.profile_id = Some("liz".into());
        let mut shared_memory = memory("shared", "The household shops at Carrefour.", 0.9, 1);
        shared_memory.profile_id = None;
        memories.extend([lizs_memory, shared_memory]);
        let mut lizs_interest = interest("Rally", true, false);
        lizs_interest.profile_id = "liz".into();
        let mut lizs_trip = trip.clone();
        lizs_trip.id = "hers".into();
        lizs_trip.profile_id = "liz".into();
        lizs_trip.title = "Kisumu alone".into();

        let listed = sources(
            "jerry",
            SourceRows {
                memories,
                summaries: &[ConversationSummary {
                    session_id: "s1".into(),
                    title: "Car hunt".into(),
                    summary: "Compared Fielder listings under KSh 1.2M.".into(),
                    updated_at: now(),
                }],
                interests: &[
                    interest("Formula 1", true, false),
                    interest("Primetime", false, false),
                    interest("Celebrity gossip", true, true),
                    lizs_interest,
                ],
                anchors: &[trip, candidate, everyday, lizs_trip],
                answers: &[asked, open],
            },
            chrono_tz::Africa::Nairobi,
            now(),
        );
        let texts: Vec<(&str, &str)> = listed
            .iter()
            .map(|s| (source_word(s.kind), s.text.as_str()))
            .collect();
        assert_eq!(
            texts,
            [
                ("memory", "Is allergic to peanuts."),
                ("memory", "Lives in Kilimani."),
                (
                    "conversation",
                    "Car hunt: Compared Fielder listings under KSh 1.2M."
                ),
                ("interest", "Follows Formula 1 (keen)"),
                ("interest", "Does not want to hear about Celebrity gossip"),
                (
                    "plan",
                    "A trip: Mombasa with Liz, 20 December 2026 to 23 December 2026, at Diani; \
                     Budget: KSh 80,000"
                ),
                (
                    "answer",
                    "Asked \"How do you get to work?\", they answered: Drive; a Fielder"
                ),
            ]
        );
        assert_eq!(listed[0].reference, "weighty");
    }

    #[test]
    fn the_message_shows_sources_sheet_removed_lines_tools_and_rules() {
        let mut own =
            FactLine::written_by_member("jerry", FactCategory::Home, "Lives in Kilimani", now())
                .unwrap();
        own.tools = vec!["get_weather".into()];
        let mut removed = held(FactCategory::Food, "Likes duck recipes", Confidence::Said);
        removed.state = FactState::Removed;
        let mut retired = held(FactCategory::Other, "Once owned a boat", Confidence::Said);
        retired.state = FactState::Retired;
        let never = DontRule::new(
            "jerry",
            "recipes built on a colour preference",
            Strength::Never,
            RuleOrigin::Reject,
            now(),
        )
        .unwrap();
        let message = build_message(
            &three_sources(),
            &[
                own,
                held(FactCategory::Work, "Runs Jarida", Confidence::Sure),
                removed,
                retired,
            ],
            &tools(),
            &[never],
        );
        assert!(message.starts_with("Sources:\n1. (memory) Runs Jarida"));
        assert!(message.contains("\n2. (answer) Asked"));
        assert!(message.contains("\n3. (interest) Follows Formula 1"));
        assert!(message.contains("- [home] Lives in Kilimani (theirs) -> get_weather\n"));
        assert!(message.contains("- [work] Runs Jarida\n"));
        assert!(message.contains("never write these again):\n- Likes duck recipes\n"));
        assert!(!message.contains("boat"), "a retired line is not the sheet");
        assert!(message.contains("- get_weather: The forecast for a place\n"));
        assert!(message.contains("- Never: recipes built on a colour preference\n"));
        assert!(message.ends_with("JSON:"));

        let bare = build_message(&[], &[], &[], &[]);
        assert!(bare.contains("Sources:\nNone yet.\n"));
        assert!(bare.contains("Current sheet:\nEmpty.\n"));
        assert!(bare.contains("Tools:\nNone.\n"));
        assert!(!bare.contains("rules"));
    }

    #[test]
    fn a_line_is_kept_only_with_a_source_that_exists_and_takes_the_first_as_its_own() {
        let raw = r#"```json
        {"lines": [
          {"category": "home", "fact": "Lives in Kilimani", "confidence": "sure",
           "since": "2019", "sources": [9, "2", 1, 2], "tools": ["Get_Weather", "book_flight"]},
          {"category": "work", "fact": "Runs Jarida", "confidence": "likely", "sources": [1]},
          {"category": "food", "fact": "Loves duck", "confidence": "sure", "sources": []},
          {"category": "food", "fact": "Loves teal food", "sources": [0, 4, "x"]},
          {"category": "work", "fact": "runs  JARIDA!", "sources": [1]},
          {"category": "astrology", "fact": "Follows Formula 1", "source": 3}
        ]}
        ```"#;
        let sources = three_sources();
        let outcome = parse(raw, &sources, &tools(), "jerry", now());
        assert!(!outcome.unparseable);
        assert_eq!(
            outcome.refused,
            [Refusal::NoSource, Refusal::NoSource, Refusal::Repeat]
        );
        assert_eq!(outcome.tools_removed, 1);

        let facts: Vec<&str> = outcome
            .drafted
            .iter()
            .map(|d| d.line.fact.as_str())
            .collect();
        assert_eq!(
            facts,
            ["Lives in Kilimani", "Runs Jarida", "Follows Formula 1"]
        );
        let home = &outcome.drafted[0];
        assert_eq!(home.cites, [1, 0], "numbers that exist, each once");
        assert_eq!(home.line.source.kind, FactSourceKind::Answer);
        assert_eq!(home.line.source.reference.as_deref(), Some("q1"));
        assert_eq!(home.line.tools, ["get_weather"], "this pond's spelling");
        assert_eq!(home.line.since.as_deref(), Some("2019"));
        assert_eq!(home.line.confidence, Confidence::Sure);
        assert_eq!(home.line.profile_id, "jerry");
        assert_eq!(home.line.state, FactState::Active);

        let work = &outcome.drafted[1];
        assert!(
            home.line.score > work.line.score,
            "sure, twice sourced and recent outranks likely, once and old"
        );
        let formula = &outcome.drafted[2];
        assert_eq!(formula.line.category, FactCategory::Other);
        assert_eq!(formula.line.confidence, Confidence::Likely);
        assert_eq!(formula.line.source.kind, FactSourceKind::Interest);
        assert_eq!(outcome.lines().len(), 3);
    }

    #[test]
    fn an_empty_reply_is_told_from_one_that_cannot_be_read() {
        let none = parse(
            r#"{"lines": []}"#,
            &three_sources(),
            &tools(),
            "jerry",
            now(),
        );
        assert_eq!(none, Outcome::default());
        let bare = parse(
            r#"[{"category":"work","fact":"Runs Jarida","sources":[1]}]"#,
            &three_sources(),
            &tools(),
            "jerry",
            now(),
        );
        assert_eq!(bare.drafted.len(), 1, "a bare list reads too");
        for raw in ["I could not find anything.", r#"{"lines": [{"fact": "Runs"#] {
            let unread = parse(raw, &three_sources(), &tools(), "jerry", now());
            assert!(unread.unparseable, "{raw}");
            assert!(unread.drafted.is_empty());
        }
    }

    #[test]
    fn a_reply_past_the_limit_keeps_the_best_hundred() {
        let lines: Vec<serde_json::Value> = (0..FACT_SHEET_LIMIT + 7)
            .map(|i| {
                serde_json::json!({
                    "category": "other",
                    "fact": format!("Fact number {i}"),
                    "confidence": if i < 7 { "said" } else { "sure" },
                    "sources": [1],
                })
            })
            .collect();
        let raw = serde_json::json!({ "lines": lines }).to_string();
        let outcome = parse(&raw, &three_sources(), &tools(), "jerry", now());
        assert_eq!(outcome.drafted.len(), FACT_SHEET_LIMIT);
        assert_eq!(outcome.refused, [Refusal::OverLimit; 7]);
        assert!(outcome
            .drafted
            .iter()
            .all(|d| d.line.confidence == Confidence::Sure));
    }

    #[test]
    fn a_rebuilt_line_keeps_the_credit_of_its_use() {
        let mut ridden = held(FactCategory::Work, "Runs Jarida", Confidence::Likely);
        ridden.used = 6;
        let fresh = vec![
            held(FactCategory::Work, "runs jarida", Confidence::Likely),
            held(FactCategory::Home, "Lives in Kilimani", Confidence::Likely),
        ];
        let scored = counting_use(fresh, &[ridden]);
        assert!(scored[0].score > scored[1].score);
        assert_eq!(scored[1].score, 0.0);
    }

    fn a_draft() -> (Vec<Drafted>, Vec<Source>) {
        let sources = three_sources();
        let raw = r#"{"lines": [
          {"category": "work", "fact": "Runs Jarida", "confidence": "sure", "sources": [1]},
          {"category": "home", "fact": "Lives in Kilimani", "sources": [2, 1]},
          {"category": "interest", "fact": "Follows Formula 1", "sources": [3]}
        ]}"#;
        let outcome = parse(raw, &sources, &tools(), "jerry", now());
        (outcome.drafted, sources)
    }

    #[test]
    fn the_critic_sees_each_line_with_the_sources_under_it() {
        let (drafted, sources) = a_draft();
        let message = build_critic_message(&drafted, &sources);
        assert!(message.starts_with(
            "Draft:\n1. [work] Runs Jarida (sure)\n   - memory: Runs Jarida, which builds"
        ));
        assert!(message.contains(
            "2. [home] Lives in Kilimani (likely)\n   - answer: Asked \"Where do you live?\""
        ));
        assert!(message.contains("\n   - memory: Runs Jarida, which builds an offline"));
        assert!(message.ends_with("JSON:"));
        assert!(build_critic_message(&[], &sources).contains("No lines."));
    }

    #[test]
    fn the_critics_drops_name_lines_of_the_draft_and_nothing_else() {
        let (drafted, _) = a_draft();
        let drops = parse_drops(
            r#"{"drop": [{"line": 3, "why": "asked  once"}, {"line": "1"}, {"line": 3},
                         {"line": 0}, {"line": 9, "why": "no such line"}, 2.5]}"#,
            drafted.len(),
        )
        .unwrap();
        assert_eq!(
            drops,
            [(0, String::new()), (2, "asked once".to_string())],
            "numbers of the draft, each once, lowest first"
        );
        let kept = surviving(drafted.clone(), &drops);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].fact, "Lives in Kilimani");

        assert_eq!(parse_drops(r#"{"drop": []}"#, 3), Some(vec![]));
        assert_eq!(parse_drops("[2]", 3), Some(vec![(1, String::new())]));
        assert_eq!(surviving(drafted, &[]).len(), 3);
    }

    #[test]
    fn a_critic_reply_that_cannot_be_read_drops_nothing_and_says_so() {
        for raw in [
            "All of these look right to me.",
            r#"{"verdict": "pass"}"#,
            r#"{"drop": [{"line": 1, "why": "cut"#,
        ] {
            assert_eq!(parse_drops(raw, 3), None, "{raw}");
        }
    }

    fn a_sheet() -> Vec<FactLine> {
        let mut home = held(FactCategory::Home, "Lives in Kilimani", Confidence::Sure);
        home.tools = vec!["get_weather".into(), "search_web".into()];
        let mut talk = held(
            FactCategory::Plan,
            "Speaking at the MCP Dev Summit in Nairobi",
            Confidence::Likely,
        );
        talk.since = Some("October".into());
        talk.updated_at = now() + Duration::hours(2);
        let mut gone = held(FactCategory::Food, "Likes duck recipes", Confidence::Said);
        gone.state = FactState::Removed;
        vec![
            held(FactCategory::Work, "Runs Jarida.", Confidence::Sure),
            home,
            talk,
            held(
                FactCategory::Hunt,
                "A Toyota Fielder under KSh 1.2M",
                Confidence::Sure,
            ),
            held(
                FactCategory::Interest,
                "Formula 1, Ferrari",
                Confidence::Sure,
            ),
            held(FactCategory::Style, "Teal", Confidence::Said),
            held(
                FactCategory::Routine,
                "Waters the greenhouse before work",
                Confidence::Sure,
            ),
            gone,
        ]
    }

    #[test]
    fn the_whole_sheet_reads_a_fact_to_a_line_grouped_by_category() {
        assert_eq!(
            sheet_for_brief(&a_sheet()),
            "Home: Lives in Kilimani -> get_weather, search_web\n\
             Work: Runs Jarida.\n\
             Plan: Speaking at the MCP Dev Summit in Nairobi (since October; likely)\n\
             Looking for: A Toyota Fielder under KSh 1.2M\n\
             Interest: Formula 1, Ferrari\n\
             Taste: Teal (said once)\n\
             Routine: Waters the greenhouse before work"
        );
        assert_eq!(sheet_for_brief(&[]), "");
    }

    #[test]
    fn the_portrait_is_the_sheet_under_its_old_headings() {
        let portrait = as_portrait(&a_sheet()).unwrap();
        assert_eq!(portrait.summary, "Runs Jarida. Lives in Kilimani.");
        assert_eq!(portrait.into, ["Formula 1, Ferrari"]);
        assert_eq!(
            portrait.working_on,
            [
                "Speaking at the MCP Dev Summit in Nairobi",
                "A Toyota Fielder under KSh 1.2M"
            ]
        );
        assert_eq!(portrait.likes, ["Teal"], "a routine is not a taste");
        assert_eq!(portrait.updated_at, now() + Duration::hours(2));

        let mut only_removed = a_sheet();
        only_removed.retain(|l| l.state == FactState::Removed);
        assert_eq!(as_portrait(&only_removed), None);
        assert_eq!(as_portrait(&[]), None);
    }

    #[test]
    fn a_member_is_described_by_the_sheet_and_only_the_interests_they_confirmed() {
        let about = about_from_sheet(
            &a_sheet(),
            &[
                interest("The Boys", true, false),
                interest("Primetime", false, false),
                interest("Celebrity gossip", true, true),
            ],
        )
        .unwrap();
        assert!(about.starts_with("Runs Jarida. Lives in Kilimani.\nInto: Formula 1, Ferrari"));
        assert!(about.contains("\nConfirmed interests: The Boys\n"));
        assert!(!about.contains("Primetime"), "a guess feeds no prompt");
        assert!(about.ends_with("Never raise: Celebrity gossip"));

        assert_eq!(
            about_from_sheet(&[], &[interest("The Boys", true, false)]).as_deref(),
            Some("Confirmed interests: The Boys")
        );
        assert_eq!(
            about_from_sheet(&[], &[interest("Primetime", false, false)]),
            None
        );
    }
}
