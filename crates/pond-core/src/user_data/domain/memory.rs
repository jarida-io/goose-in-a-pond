//! Memory fragments, their classification and decay tiers, and the fact quality gate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

use crate::user_data::domain::member_profile::About;

// ── Memory classification ────────────────────────────────────────────────────

/// Semantic category of a memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySegment {
    /// Core facts about the user's identity (name, role, location).
    Identity,
    /// How the user likes things done (style, defaults, preferences).
    Preference,
    /// Corrections the user made to the assistant's knowledge.
    Correction,
    /// People the user knows — family, friends, colleagues.
    Relationship,
    /// Ongoing tasks, goals, work projects.
    Project,
    /// A habit, a standing way the user does things; only batch extraction can see one.
    Routine,
    /// Facts about the world, not the member. Kept for rows filed before the profile; nothing
    /// new is filed here.
    Knowledge,
    /// Transient context (current situation, ongoing state).
    Context,
    /// What they eat and what their body needs: diet, allergies, health.
    FoodHealth,
    /// A place that matters to them.
    Place,
    /// A birthday or anniversary that comes round each year.
    Date,
    /// A standing instruction to the assistant.
    Instruction,
}

impl MemorySegment {
    /// Canonical segment tag.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Preference => "preference",
            Self::Correction => "correction",
            Self::Relationship => "relationship",
            Self::Project => "project",
            Self::Routine => "routine",
            Self::Knowledge => "knowledge",
            Self::Context => "context",
            Self::FoodHealth => "food_health",
            Self::Place => "place",
            Self::Date => "date",
            Self::Instruction => "instruction",
        }
    }

    /// One of the canonical tags.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|s| s.as_str() == raw.trim().to_ascii_lowercase())
            .cloned()
    }

    /// Every segment, in the order a member's profile shows them.
    pub const ALL: [MemorySegment; 12] = [
        Self::Instruction,
        Self::Correction,
        Self::Identity,
        Self::Relationship,
        Self::FoodHealth,
        Self::Routine,
        Self::Preference,
        Self::Project,
        Self::Place,
        Self::Date,
        Self::Context,
        Self::Knowledge,
    ];

    /// Where this segment comes in a member's profile.
    pub fn profile_rank(&self) -> usize {
        Self::ALL
            .iter()
            .position(|s| s == self)
            .unwrap_or(Self::ALL.len())
    }

    /// The heading a person, and the model, reads this segment under.
    pub fn heading(&self) -> &'static str {
        match self {
            Self::Instruction => "Standing instructions",
            Self::Correction => "Corrections",
            Self::Identity => "Who they are",
            Self::Relationship => "People",
            Self::FoodHealth => "Food and health",
            Self::Routine => "Routines",
            Self::Preference => "Preferences",
            Self::Project => "Goals",
            Self::Place => "Places",
            Self::Date => "Dates",
            Self::Context => "Right now",
            Self::Knowledge => "Other",
        }
    }

    /// The heading as the member reads it on their own page.
    pub fn heading_to_member(&self) -> &'static str {
        match self {
            Self::Identity => "Who you are",
            other => other.heading(),
        }
    }

    /// Default importance for this segment (0.0–1.0).
    pub fn default_importance(&self) -> f32 {
        match self {
            Self::Correction | Self::Instruction => 0.9,
            Self::FoodHealth => 0.85,
            Self::Identity | Self::Date => 0.8,
            Self::Preference => 0.7,
            Self::Relationship => 0.7,
            // Below a stated preference (it's inferred), above a project (it's more durable).
            Self::Routine => 0.65,
            Self::Project | Self::Place => 0.6,
            Self::Knowledge => 0.5,
            Self::Context => 0.3,
        }
    }

    pub fn default_tier(&self) -> MemoryTier {
        match self {
            Self::Identity | Self::Instruction | Self::Date => MemoryTier::Permanent,
            Self::Correction => MemoryTier::Long,
            Self::Context => MemoryTier::Short,
            _ => MemoryTier::Long,
        }
    }
}

/// Lifecycle tier controlling decay behaviour.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    /// High decay rate — expected to expire within days.
    Short,
    /// Moderate decay — retained for weeks/months.
    Long,
    /// Never decays, never pruned.
    Permanent,
}

impl MemoryTier {
    /// Default decay rate (lambda) for this tier.
    pub fn default_decay_rate(&self) -> f32 {
        match self {
            Self::Short => 0.10,
            Self::Long => 0.01,
            Self::Permanent => 0.00,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLifecycle {
    /// Normal operational state — included in searches.
    Active,
    /// Below archive threshold — hidden from recall but not deleted.
    Archived,
    /// Consolidated into another memory.
    Merged,
}

// ── Memory fragment ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFragment {
    pub id: String,
    /// Profile this memory belongs to (None = global)
    pub profile_id: Option<String>,
    /// Session this memory was extracted from (None = manual/external)
    pub session_id: Option<String>,
    pub content: String,
    /// Raw embedding vector (None until an EmbeddingProvider generates it)
    #[serde(skip)]
    pub embedding: Option<Vec<f32>>,
    /// Source of this fragment: "chat", "note", "sensor_summary", "extraction", "mcp_tool"
    pub source: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,

    // ── Segment-aware fields (all optional for backward compat) ───────────
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment: Option<MemorySegment>,
    /// Importance score (0.0–1.0). Higher = more worth retaining.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importance: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<MemoryTier>,
    /// Decay rate (lambda). Defaults from tier if absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decay_rate: Option<f32>,
    /// Number of times this memory has been accessed (recalled or injected).
    #[serde(default)]
    pub access_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_accessed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<MemoryLifecycle>,
    /// ID of the memory that superseded this one (via consolidation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// For corrections: the wrong claim this fixes, so consolidation never reverts it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrects: Option<String>,
    /// What it says about its member; `None` on a row filed before the profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<About>,
    /// When a passing fact stops being shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<DateTime<Utc>>,
}

impl MemoryFragment {
    pub fn from_chat(
        id: String,
        profile_id: Option<String>,
        session_id: Option<String>,
        content: String,
    ) -> Self {
        Self {
            id,
            profile_id,
            session_id,
            content,
            embedding: None,
            source: "chat".to_string(),
            tags: vec![],
            created_at: Utc::now(),
            segment: None,
            importance: None,
            tier: None,
            decay_rate: None,
            access_count: 0,
            last_accessed_at: None,
            lifecycle: None,
            superseded_by: None,
            corrects: None,
            about: None,
            valid_until: None,
        }
    }

    /// Whether a turn may be shown this row at `now`: live and not past its time.
    pub fn is_shown(&self, now: DateTime<Utc>) -> bool {
        !matches!(
            self.lifecycle,
            Some(MemoryLifecycle::Archived | MemoryLifecycle::Merged)
        ) && self.superseded_by.is_none()
            && self.valid_until.is_none_or(|until| until > now)
    }

    /// True for user corrections, which consolidation must never prune or merge away.
    pub fn is_correction(&self) -> bool {
        self.segment.as_ref() == Some(&MemorySegment::Correction) || self.corrects.is_some()
    }

    /// True for the permanent tier, which decay never archives and consolidation never prunes.
    pub fn is_kept(&self) -> bool {
        self.tier.as_ref() == Some(&MemoryTier::Permanent)
    }

    /// Move to the permanent tier, with the decay rate that tier implies.
    pub fn keep(&mut self) {
        self.tier = Some(MemoryTier::Permanent);
        self.decay_rate = Some(MemoryTier::Permanent.default_decay_rate());
    }

    /// Pass `corrects` for `Correction` segments so consolidation cannot revert the fix.
    pub fn from_extraction(
        id: String,
        session_id: Option<String>,
        content: String,
        segment: MemorySegment,
        importance: f32,
        corrects: Option<String>,
    ) -> Self {
        let tier = segment.default_tier();
        let decay_rate = tier.default_decay_rate();
        Self {
            id,
            profile_id: None,
            session_id,
            content,
            embedding: None,
            source: "extraction".to_string(),
            tags: vec![],
            created_at: Utc::now(),
            segment: Some(segment),
            importance: Some(importance),
            tier: Some(tier),
            decay_rate: Some(decay_rate),
            access_count: 0,
            last_accessed_at: None,
            lifecycle: Some(MemoryLifecycle::Active),
            superseded_by: None,
            corrects,
            about: None,
            valid_until: None,
        }
    }

    /// Create a fragment from one window of batch extraction.
    /// Takes `tier` because the segment default would make `Identity` facts never decay.
    pub fn from_window_extraction(
        id: String,
        profile_id: Option<String>,
        session_id: Option<String>,
        content: String,
        segment: MemorySegment,
        importance: f32,
        tier: MemoryTier,
    ) -> Self {
        let decay_rate = tier.default_decay_rate();
        Self {
            id,
            profile_id,
            session_id,
            content,
            embedding: None,
            source: "extraction".to_string(),
            tags: vec![],
            created_at: Utc::now(),
            segment: Some(segment),
            importance: Some(importance),
            tier: Some(tier),
            decay_rate: Some(decay_rate),
            access_count: 0,
            last_accessed_at: None,
            lifecycle: Some(MemoryLifecycle::Active),
            superseded_by: None,
            corrects: None,
            about: None,
            valid_until: None,
        }
    }
}

// ── Fact quality gate ───────────────────────────────────────────────────────

/// Shortest trimmed content, in characters, that can carry a fact.
pub const MIN_FACT_CONTENT_LEN: usize = 8;
/// Longest content, in characters, that is still one fact: past it a note is a paragraph of
/// several, which cannot be filed, corrected or forgotten one at a time.
pub const MAX_FACT_CONTENT_LEN: usize = 300;

/// Why a candidate memory was refused at write time: out of context it would mislead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactDefect {
    /// Nothing left after normalisation, or too short to carry a fact.
    TooShort,
    /// Longer than [`MAX_FACT_CONTENT_LEN`]: several facts run together.
    TooLong,
    /// A deictic or leading pronoun with no antecedent in the sentence ("the latter city").
    UnresolvedReference,
    /// First person ("my mother"): injected into context, "my" reads as the assistant's.
    FirstPerson,
    /// A verbatim copy of the extraction prompt's worked example. Small models copy examples,
    /// and these facts pass every other check, so they are refused outright.
    EchoedExample,
    /// The note carries a calendar date, so it is refused whole: rewriting one mangles it.
    /// Read back months later a dated fact misleads; dates belong in reminders, which expire.
    CalendarDate,
    /// The sentence stops on a word leading into something else ("The user's cat is called").
    Fragment,
}

impl FactDefect {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TooShort => "too short",
            Self::TooLong => "more than one fact",
            Self::UnresolvedReference => "unresolved reference",
            Self::FirstPerson => "first person",
            Self::EchoedExample => "echoed the prompt's own example",
            Self::CalendarDate => "carries a calendar date",
            Self::Fragment => "stops mid-sentence",
        }
    }
}

impl std::fmt::Display for FactDefect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Labels a small model prepends ("Active Project: …"); matched case-insensitively.
const LABEL_PREFIXES: &[&str] = &[
    "active project",
    "current project",
    "ongoing project",
    "project",
    "preference",
    "relationship",
    "correction",
    "knowledge",
    "identity",
    "context",
    "memory",
    "fact",
    "note",
];

/// How far into the string a colon may sit and still be a label separator.
const LABEL_SCAN_CHARS: usize = 24;

/// Verbs that can open a captured request; not sufficient alone, see [`is_captured_request`].
const TASK_VERBS: &[&str] = &[
    "add",
    "build",
    "calculate",
    "check",
    "compile",
    "convert",
    "create",
    "debug",
    "delete",
    "design",
    "draft",
    "explain",
    "find",
    "fix",
    "generate",
    "give",
    "help",
    "implement",
    "install",
    "list",
    "make",
    "open",
    "play",
    "refactor",
    "remind",
    "remove",
    "rename",
    "run",
    "schedule",
    "send",
    "set",
    "show",
    "summarise",
    "summarize",
    "tell",
    "translate",
    "turn",
    "update",
    "write",
];

/// Objects of finished assistant work; deliberately concrete ("reminder", not "novel").
const ASSISTANT_ARTIFACT_NOUNS: &[&str] = &[
    "alarm",
    "appointment",
    "calendar",
    "chart",
    "code",
    "command",
    "draft",
    "email",
    "file",
    "folder",
    "function",
    "list",
    "meeting",
    "message",
    "note",
    "password",
    "playlist",
    "program",
    "query",
    "regex",
    "reminder",
    "screenshot",
    "script",
    "snippet",
    "spreadsheet",
    "summary",
    "timer",
    "translation",
];

/// Unambiguous first-person markers ("i"/"us" handled separately). "mine" is left out on
/// purpose: it is usually a noun ("coal mine"); missing the pronoun is the cheaper error.
const FIRST_PERSON: &[&str] = &["my", "myself", "our", "ours", "ourselves", "we", "me"];

/// Contracted first-person forms, listed since [`split_tokens`] keeps "I'm" as one token.
const FIRST_PERSON_CONTRACTIONS: &[&str] = &[
    "i'm", "i've", "i'll", "i'd", "we're", "we've", "we'll", "we'd", "let's",
];

/// Tokens after "I" that make it a pronoun, not a numeral ("Type I diabetes" must survive).
const I_PREDICATES: &[&str] = &[
    "am", "was", "have", "had", "will", "would", "can", "could", "should", "do", "did", "like",
    "prefer", "want", "need", "think", "live", "work", "use", "enjoy", "hate", "love", "also",
    "just", "usually", "always", "never", "often",
];

/// "the latter"/"the former" select between two candidates, so they need two antecedents.
const CONTRASTIVE_ANTECEDENTS: usize = 2;

/// Pronouns that cannot resolve when they open a sentence.
const LEADING_PRONOUNS: &[&str] = &[
    "he", "she", "they", "him", "her", "them", "his", "hers", "its", "it", "their", "theirs",
];

/// Bigram deictics that point outside the sentence.
const DEICTIC_BIGRAMS: &[(&str, &str)] = &[
    ("that", "place"),
    ("this", "place"),
    ("same", "place"),
    ("that", "city"),
    ("that", "town"),
    ("that", "country"),
    ("that", "person"),
    ("that", "one"),
];

/// Verbs after "there" that make it the expletive subject ("there is a leak"), not a place.
const EXPLETIVE_FOLLOWERS: &[&str] = &[
    "is", "are", "was", "were", "will", "would", "has", "have", "had", "seems", "appears",
];

/// Splits into (raw, lowercase) tokens, trimming only edge punctuation so "user's" stays whole.
fn split_tokens(content: &str) -> Vec<(&str, String)> {
    content
        .split_whitespace()
        .map(|raw| raw.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|t| !t.is_empty())
        .map(|t| (t, t.to_lowercase()))
        .collect()
}

/// Collapse whitespace and drop a leading label the model invented ("Project: …").
pub fn normalise_fact_content(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(colon) = collapsed
        .char_indices()
        .take(LABEL_SCAN_CHARS)
        .find(|(_, c)| *c == ':')
        .map(|(i, _)| i)
    else {
        return collapsed;
    };
    let label = collapsed[..colon].trim().to_lowercase();
    if LABEL_PREFIXES.contains(&label.as_str()) {
        collapsed[colon + 1..].trim().to_string()
    } else {
        collapsed
    }
}

/// First defect that makes content unstorable. Every rule discards facts for good, so each
/// matches only patterns a well-formed third-person sentence cannot produce.
pub fn fact_defect(content: &str) -> Option<FactDefect> {
    let trimmed = content.trim();
    if trimmed.chars().count() < MIN_FACT_CONTENT_LEN {
        return Some(FactDefect::TooShort);
    }
    if trimmed.chars().count() > MAX_FACT_CONTENT_LEN {
        return Some(FactDefect::TooLong);
    }
    let tokens = split_tokens(trimmed);
    if tokens.is_empty() {
        return Some(FactDefect::TooShort);
    }
    if has_first_person(&tokens) {
        return Some(FactDefect::FirstPerson);
    }
    if has_unresolved_reference(&tokens) {
        return Some(FactDefect::UnresolvedReference);
    }
    if is_extraction_example(trimmed) {
        return Some(FactDefect::EchoedExample);
    }
    if carries_calendar_date(trimmed) {
        return Some(FactDefect::CalendarDate);
    }
    // After the date rung, so a dated sentence is refused for its date, not its ending.
    if tokens.last().is_some_and(|(_, lower)| {
        DANGLING_TAIL_WORDS.contains(&lower.as_str())
            || DANGLING_TAIL_VERBS.contains(&lower.as_str())
    }) {
        return Some(FactDefect::Fragment);
    }
    None
}

/// Words a finished sentence does not end on. Keep it short: each entry discards facts for good.
const DANGLING_TAIL_WORDS: &[&str] = &[
    "is", "are", "was", "were", "be", "been", "being", "am", "and", "or", "but", "of", "on", "in",
    "at", "to", "by", "with", "for", "from", "into", "the", "a", "an", "every", "each", "about",
    "than", "as", "that", "which", "who", "until", "till", "since", "during", "within", "around",
    "before", "after",
];

/// Verbs a clause does not end on: each only introduces the word that is missing.
const DANGLING_TAIL_VERBS: &[&str] = &[
    "called",
    "named",
    "nicknamed",
    "spelled",
    "become",
    "becomes",
    "became",
];

// ── Dates ───────────────────────────────────────────────────────────────────
// Detect, never rewrite: whether a sentence survives losing its date is a question of meaning.
// A false positive costs a good fact, so rules cover only shapes models were measured to leak.

/// Month names and the abbreviations a model actually writes.
const MONTH_WORDS: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "jan",
    "feb",
    "mar",
    "apr",
    "jun",
    "jul",
    "aug",
    "sept",
    "sep",
    "oct",
    "nov",
    "dec",
];

/// Usually the modal: a date only after a preposition or beside a day number ("in May", "3 May").
const AMBIGUOUS_MONTH: &str = "may";

const WEEKDAY_WORDS: &[&str] = &[
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// Parts of the week: never a date alone ("works at the weekend"), only part of a recurrence.
const PERIOD_WORDS: &[&str] = &["weekday", "weekend"];

/// Weekday abbreviations, kept out of [`WEEKDAY_WORDS`]: "sat" and "sun" are ordinary words.
/// Consulted only where context fixes the reading ("next sat", "from Mon to Fri").
const WEEKDAY_ABBREVIATIONS: &[&str] = &[
    "mon", "tue", "tues", "wed", "weds", "thu", "thur", "thurs", "fri", "sat", "sun",
];

/// Single-word relative days; not "midnight"/"noon", ordinary nouns models never leaked.
const RELATIVE_WORDS: &[&str] = &["today", "tomorrow", "yesterday", "tonight", "overmorrow"];

const RELATIVE_HEADS: &[&str] = &["next", "last", "this", "coming", "past"];
/// Words that turn a preceding "next", "last" or "this" into a date.
const RELATIVE_TAILS: &[&str] = &[
    "week",
    "weekend",
    "month",
    "year",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// Prepositions that put a date after them.
const DATE_PREPOSITIONS: &[&str] = &["on", "in", "at", "by", "since", "until", "till", "from"];

/// Spelled days of the month; a date only by the words either side ("on the first").
const ORDINAL_WORDS: &[&str] = &[
    "first",
    "second",
    "third",
    "fourth",
    "fifth",
    "sixth",
    "seventh",
    "eighth",
    "ninth",
    "tenth",
    "eleventh",
    "twelfth",
    "thirteenth",
    "fourteenth",
    "fifteenth",
    "sixteenth",
    "seventeenth",
    "eighteenth",
    "nineteenth",
    "twentieth",
    "twenty-first",
    "twenty-second",
    "twenty-third",
    "twenty-fourth",
    "twenty-fifth",
    "twenty-sixth",
    "twenty-seventh",
    "twenty-eighth",
    "twenty-ninth",
    "thirtieth",
    "thirty-first",
];

/// Spelled clock hours; a date only after a [`CLOCK_LEADS`] word ("at six", not "six chickens").
const HOUR_WORDS: &[&str] = &[
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
    "twelve",
];

/// Units that turn a count into a stretch of calendar time ("in three weeks").
const DURATION_UNITS: &[&str] = &[
    "day",
    "days",
    "week",
    "weeks",
    "fortnight",
    "month",
    "months",
    "year",
    "years",
];

/// What leads into a clock time.
const CLOCK_LEADS: &[&str] = &[
    "at", "by", "around", "before", "after", "from", "until", "till",
];

/// What leads into a counted stretch of time ("in three weeks", not "in three offices").
const DURATION_LEADS: &[&str] = &["in", "within"];

// ── Recurrence ──────────────────────────────────────────────────────────────

/// Words that make a weekday, month or clock time a pattern, not a calendar point.
const RECURRENCE_MARKERS: &[&str] = &["each", "every", "daily", "nightly", "weekly", "monthly"];

/// Joins recurrence items ("each March and October"); only read right after a recurring token.
const RECURRENCE_CONNECTIVES: &[&str] = &["and", "or", "to", "through", "thru"];

/// Whether this token names a weekday, month or week-part (for the recurrence rules).
fn is_calendar_name(lower: &str) -> bool {
    WEEKDAY_WORDS.contains(&lower) || MONTH_WORDS.contains(&lower) || PERIOD_WORDS.contains(&lower)
}

/// Whether this token names a weekday or month: a calendar point unless marked as recurring.
fn is_day_or_month_name(lower: &str) -> bool {
    WEEKDAY_WORDS.contains(&lower) || MONTH_WORDS.contains(&lower)
}

/// `is_calendar_name` plus [`WEEKDAY_ABBREVIATIONS`], for callers that already fixed the reading.
fn is_calendar_name_or_abbrev(lower: &str) -> bool {
    is_calendar_name(lower) || WEEKDAY_ABBREVIATIONS.contains(&lower)
}

/// A plural calendar name ("Saturdays", "weekdays"), which recurs without a marker.
fn is_plural_calendar_name(lower: &str) -> bool {
    lower
        .strip_suffix('s')
        .is_some_and(|stem| stem.len() > 3 && is_calendar_name(stem))
}

/// Which token positions name a weekday or month that recurs: a habit, not an appointment.
/// A numbered day never recurs, even "the 1st of every month": that is a reminder's day.
fn recurrence_positions(words: &[String]) -> Vec<bool> {
    let mut out = vec![false; words.len()];
    let at = |j: usize| words.get(j).map(String::as_str);
    for i in 0..words.len() {
        let here = words[i].as_str();
        if is_plural_calendar_name(here) {
            out[i] = true;
            continue;
        }
        if !is_calendar_name_or_abbrev(here) {
            continue;
        }
        let before = i.checked_sub(1).and_then(at);
        // "each Saturday", "every other Monday".
        if before.is_some_and(|b| RECURRENCE_MARKERS.contains(&b))
            || (before == Some("other")
                && i.checked_sub(2)
                    .and_then(at)
                    .is_some_and(|b| RECURRENCE_MARKERS.contains(&b)))
        {
            out[i] = true;
            continue;
        }
        // "from Monday to Friday", "Mon to Fri": the frame is what makes an abbreviation safe.
        if at(i + 1) == Some("to") && at(i + 2).is_some_and(is_calendar_name_or_abbrev) {
            out[i] = true;
            out[i + 2] = true;
            continue;
        }
        // "…each March AND OCTOBER", only immediately after one already found.
        if before.is_some_and(|b| RECURRENCE_CONNECTIVES.contains(&b))
            && i.checked_sub(2).is_some_and(|j| out[j])
        {
            out[i] = true;
        }
    }
    out
}

/// Whether the sentence is a habit; used only to spare clock times ("chai at six every day").
fn is_habitual(words: &[String], recurring: &[bool]) -> bool {
    recurring.iter().any(|r| *r)
        || words
            .iter()
            .any(|w| RECURRENCE_MARKERS.contains(&w.as_str()))
}

/// Whether a note carries a calendar date, and therefore cannot be stored.
/// Each `true` discards a fact, so rules match only shapes models were measured to leak.
pub fn carries_calendar_date(content: &str) -> bool {
    let raw: Vec<&str> = content.split_whitespace().collect();
    let words: Vec<String> = raw.iter().map(|w| bare_token(w)).collect();
    let recurring = recurrence_positions(&words);
    let habitual = is_habitual(&words, &recurring);

    for (i, word) in words.iter().enumerate() {
        if recurring[i] || word.is_empty() {
            continue;
        }
        let w = word.as_str();
        let prev = i.checked_sub(1).map(|j| words[j].as_str());
        let next = words.get(i + 1).map(String::as_str);
        let next2 = words.get(i + 2).map(String::as_str);
        let prev_in = |set: &[&str]| prev.is_some_and(|p| set.contains(&p));
        let next_in = |set: &[&str]| next.is_some_and(|n| set.contains(&n));
        // Clause-final: "on the fourteenth." is a date, "the fourteenth row" is not.
        let closes = raw[i].ends_with(['.', ',', ';', ':', '!', '?']) || i + 1 >= raw.len();

        // A non-recurring weekday or month: an appointment.
        if is_day_or_month_name(w)
            && (w != AMBIGUOUS_MONTH
                || prev_in(DATE_PREPOSITIONS)
                || next.is_some_and(is_day_number)
                || prev.is_some_and(is_day_number))
        {
            return true;
        }
        if RELATIVE_WORDS.contains(&w) {
            return true;
        }
        // "next week", "last month", "this Friday".
        if RELATIVE_HEADS.contains(&w) && next.is_some_and(is_relative_tail) {
            return true;
        }
        // A year, decade, ISO or slash date; knowingly over-fires on a year used as a name.
        if is_year_like(w) || is_numeric_date_group(w) {
            return true;
        }
        // A numbered day: "on the 14th", or "3 November" where the month says what 3 is.
        if is_numeric_ordinal(w) || (is_day_number(w) && next_in(MONTH_WORDS)) {
            return true;
        }
        // A spelled day ("the third of May"); gated both sides, as bare ordinals are commoner.
        if ORDINAL_WORDS.contains(&w)
            && (prev_in(DATE_PREPOSITIONS) || prev == Some("the"))
            && (closes || (next == Some("of") && next2.is_some_and(|n| MONTH_WORDS.contains(&n))))
        {
            return true;
        }
        // "in three weeks", "within 10 days": a date said relatively.
        if (HOUR_WORDS.contains(&w) || is_all_digits(w))
            && prev_in(DURATION_LEADS)
            && next_in(DURATION_UNITS)
        {
            return true;
        }
        // Clock times go last, as a habit skips them; never a bare integer ("the oven at 180").
        if habitual {
            continue;
        }
        if is_clock_token(w) {
            return true;
        }
        if (HOUR_WORDS.contains(&w) || is_all_digits(w))
            && next_in(&["am", "pm", "oclock", "o'clock"])
        {
            return true;
        }
        if HOUR_WORDS.contains(&w) && prev_in(CLOCK_LEADS) {
            return true;
        }
    }
    false
}

/// One token, lowercased, with only edge punctuation trimmed: "09:00" keeps its colon.
fn bare_token(raw: &str) -> String {
    raw.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Whether a token could be a day of the month: 1 to 31, written plainly.
fn is_day_number(lower: &str) -> bool {
    lower.len() <= 2 && is_all_digits(lower) && (1..=31).contains(&lower.parse().unwrap_or(0))
}

/// Whether "next"/"last"/"this" in front of this token makes a date of it.
fn is_relative_tail(lower: &str) -> bool {
    RELATIVE_TAILS.contains(&lower) || WEEKDAY_ABBREVIATIONS.contains(&lower)
}

fn is_all_digits(lower: &str) -> bool {
    !lower.is_empty() && lower.chars().all(|c| c.is_ascii_digit())
}

/// A four-digit year this pond could plausibly be told about.
fn is_year(token: &str) -> bool {
    token.len() == 4 && is_all_digits(token) && (1900..=2099).contains(&token.parse().unwrap_or(0))
}

/// A year, or a decade built on one: "1984", "the 1990s", "the 2000's".
fn is_year_like(lower: &str) -> bool {
    is_year(lower)
        || lower
            .strip_suffix("'s")
            .or_else(|| lower.strip_suffix('s'))
            .is_some_and(is_year)
}

/// A numeric date in one token: `2027-11-03`, `14/03/1984`, `03.11.2027`, `03/11/27`.
/// Needs a year or three parts: `120/80` is a blood pressure and `2019.1` a version.
fn is_numeric_date_group(lower: &str) -> bool {
    for sep in ['-', '/', '.'] {
        if !lower.contains(sep) {
            continue;
        }
        let parts: Vec<&str> = lower.split(sep).collect();
        if !(2..=3).contains(&parts.len()) || !parts.iter().all(|p| is_all_digits(p)) {
            continue;
        }
        if parts.iter().any(|p| is_year(p)) && (sep != '.' || parts.len() == 3) {
            return true;
        }
        if parts.len() == 3 && sep != '.' && parts.iter().all(|p| p.len() <= 2) {
            return true;
        }
    }
    false
}

/// An ordinal in digits ("4th"); knowingly over-fires on floors and exam placings.
fn is_numeric_ordinal(lower: &str) -> bool {
    lower
        .strip_suffix("st")
        .or_else(|| lower.strip_suffix("nd"))
        .or_else(|| lower.strip_suffix("rd"))
        .or_else(|| lower.strip_suffix("th"))
        .is_some_and(is_all_digits)
}

/// A clock time that says so in its own spelling: "09:00", "9am", "11pm".
fn is_clock_token(lower: &str) -> bool {
    for meridiem in ["am", "pm"] {
        if let Some(hour) = lower.strip_suffix(meridiem) {
            if !hour.is_empty() && hour.len() <= 2 && is_all_digits(hour) {
                return true;
            }
        }
    }
    if let Some((h, m)) = lower.split_once(':') {
        if is_all_digits(h) && is_all_digits(m) {
            return true;
        }
    }
    false
}

/// Outputs of the extraction prompt's worked example, refused on exact (case-insensitive) echo.
/// Empty while the prompt has none; `the_prompt_and_the_echo_gate_agree_about_examples` pins it.
pub const EXTRACTION_EXAMPLE_FACTS: &[&str] = &[];

fn is_extraction_example(content: &str) -> bool {
    EXTRACTION_EXAMPLE_FACTS
        .iter()
        .any(|ex| ex.trim().eq_ignore_ascii_case(content.trim()))
}

/// Drop a trailing plural "s" so a singular-only word list matches either form.
fn singular(word: &str) -> &str {
    match word.strip_suffix('s') {
        Some(stem) if stem.len() >= 3 => stem,
        _ => word,
    }
}

/// Whether a fact mentions the user. Use it to demote, not reject: rejection loses the fact.
pub fn names_user(content: &str) -> bool {
    names_subject(content, &[])
}

/// Whether a fact names the window's subject, as "the user" or by one of `aliases`.
/// Aliases match whole tokens, since a substring would find "Al" in "also".
pub fn names_subject(content: &str, aliases: &[String]) -> bool {
    let tokens = split_tokens(content);
    tokens.iter().any(|(_, normalised)| {
        // split_tokens keeps "user's" whole, and the prompt teaches the possessive form.
        if matches!(normalised.as_str(), "user" | "users" | "user's" | "users'") {
            return true;
        }
        let stem = normalised
            .strip_suffix("'s")
            .or_else(|| normalised.strip_suffix("s'"))
            .unwrap_or(normalised);
        aliases.iter().any(|alias| {
            // Multi-word aliases match on the first word, the part a sentence repeats.
            alias
                .split_whitespace()
                .next()
                .is_some_and(|first| first.eq_ignore_ascii_case(stem))
        })
    })
}

// ── Does a reminder cover this note ─────────────────────────────────────────

/// Words too common to show two notes share a subject; an entry can only make a date count LOST.
const UNDISTINCTIVE_WORDS: &[&str] = &[
    "user",
    "have",
    "having",
    "will",
    "with",
    "that",
    "this",
    "they",
    "them",
    "their",
    "there",
    "from",
    "about",
    "into",
    "over",
    "under",
    "been",
    "being",
    "does",
    "doing",
    "done",
    "going",
    "goes",
    "went",
    "said",
    "says",
    "plan",
    "plans",
    "planned",
    "planning",
    "time",
    "times",
    "thing",
    "things",
    "some",
    "then",
    "than",
    "when",
    "what",
    "where",
    "which",
    "while",
    "also",
    "because",
    "after",
    "before",
    "again",
    "still",
    "just",
    "only",
    "very",
    "much",
    "more",
    "most",
    "other",
    "another",
    "same",
    "need",
    "needs",
    "needed",
    "want",
    "wants",
    "wanted",
    "like",
    "likes",
    "liked",
    "make",
    "makes",
    "made",
    "take",
    "takes",
    "taken",
    "took",
    "gets",
    "keep",
    "keeps",
    "kept",
    "must",
    "should",
    "would",
    "could",
    "appointment",
    "appointments",
    "reminder",
    "reminders",
    "sees",
    "seen",
    "seeing",
    "meet",
    "meets",
    "meeting",
    "meetings",
    "visit",
    "visits",
    "visiting",
];

/// Whether a token could distinguish one note from another.
/// Calendar words and the subject's aliases can't: nearly every note and reminder has them.
fn is_distinctive_token(lower: &str, aliases: &[String]) -> bool {
    if lower.len() < 4 || lower.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if UNDISTINCTIVE_WORDS.contains(&lower) {
        return false;
    }
    if MONTH_WORDS.contains(&lower)
        || WEEKDAY_WORDS.contains(&lower)
        || WEEKDAY_ABBREVIATIONS.contains(&lower)
        || PERIOD_WORDS.contains(&lower)
        || RELATIVE_WORDS.contains(&lower)
        || RELATIVE_HEADS.contains(&lower)
        || RELATIVE_TAILS.contains(&lower)
        || ORDINAL_WORDS.contains(&lower)
        || HOUR_WORDS.contains(&lower)
        || DURATION_UNITS.contains(&lower)
        || RECURRENCE_MARKERS.contains(&lower)
    {
        return false;
    }
    let stem = lower
        .strip_suffix("'s")
        .or_else(|| lower.strip_suffix("s'"))
        .unwrap_or(lower);
    !aliases.iter().any(|alias| {
        alias
            .split_whitespace()
            .next()
            .is_some_and(|first| first.eq_ignore_ascii_case(stem))
    })
}

/// A sentence's distinctive words, singularised so "tractors" matches "tractor".
fn distinctive_tokens(content: &str, aliases: &[String]) -> Vec<String> {
    split_tokens(content)
        .into_iter()
        .filter(|(_, lower)| is_distinctive_token(lower, aliases))
        .map(|(_, lower)| singular(&lower).to_string())
        .collect()
}

/// Whether a stored reminder is plausibly about this refused note: one shared distinctive word.
/// Coarse on purpose, erring toward "lost": a synonym ("surgery" for "dentist") won't match.
pub fn reminder_covers_note(about: &str, note: &str, subject_aliases: &[String]) -> bool {
    let note_words = distinctive_tokens(note, subject_aliases);
    if note_words.is_empty() {
        // Nothing to match on (only a name and a date), so the date isn't shown to be kept.
        return false;
    }
    let about_words = distinctive_tokens(about, subject_aliases);
    about_words.iter().any(|w| note_words.contains(w))
}

pub fn is_captured_request(content: &str) -> bool {
    let tokens = split_tokens(content);
    if tokens.len() < 3 || !TASK_VERBS.contains(&tokens[0].1.as_str()) {
        return false;
    }
    tokens
        .iter()
        .skip(1)
        .any(|(_, lower)| ASSISTANT_ARTIFACT_NOUNS.contains(&singular(lower)))
}

/// Fold the typographic apostrophe onto ASCII so "I’m" and "I'm" are one token.
fn ascii_apostrophe(word: &str) -> Cow<'_, str> {
    if word.contains('\u{2019}') {
        Cow::Owned(word.replace('\u{2019}', "'"))
    } else {
        Cow::Borrowed(word)
    }
}

fn has_first_person(tokens: &[(&str, String)]) -> bool {
    tokens.iter().enumerate().any(|(idx, (raw, lower))| {
        let token = ascii_apostrophe(lower);
        if FIRST_PERSON_CONTRACTIONS.contains(&token.as_ref()) {
            return true;
        }
        let prev = idx.checked_sub(1).map(|i| tokens[i].1.as_str());
        let next = tokens.get(idx + 1).map(|(_, l)| l.as_str());
        match token.as_ref() {
            "i" => idx == 0 || next.is_some_and(|n| I_PREDICATES.contains(&n)),
            // The country, spelled "US" or written as "the US", is not a pronoun.
            "us" => *raw != "US" && prev != Some("the"),
            other => FIRST_PERSON.contains(&other),
        }
    })
}

/// Proper nouns before `idx`, the only antecedents an anaphor can have. Their kind (place vs
/// person) is deliberately not checked: requiring a place dropped correct facts.
fn antecedents_before(tokens: &[(&str, String)], idx: usize) -> usize {
    (1..idx).filter(|i| is_proper_noun(tokens, *i)).count()
}

fn is_proper_noun(tokens: &[(&str, String)], idx: usize) -> bool {
    let (raw, lower) = &tokens[idx];
    lower != "i" && raw.chars().next().is_some_and(|c| c.is_uppercase())
}

fn has_unresolved_reference(tokens: &[(&str, String)]) -> bool {
    if LEADING_PRONOUNS.contains(&tokens[0].1.as_str()) {
        return true;
    }

    for (idx, (_, lower)) in tokens.iter().enumerate() {
        let prev = idx.checked_sub(1).map(|i| tokens[i].1.as_str());
        let next = tokens.get(idx + 1);
        match lower.as_str() {
            "latter" | "former" if prev == Some("the") => {
                // A capitalised next word ("the former Yugoslavia") names the referent itself.
                let names_referent = next
                    .and_then(|(raw, _)| raw.chars().next())
                    .is_some_and(|c| c.is_uppercase());
                if !names_referent && antecedents_before(tokens, idx) < CONTRASTIVE_ANTECEDENTS {
                    return true;
                }
            }
            "there" => {
                let expletive =
                    next.is_some_and(|(_, l)| EXPLETIVE_FOLLOWERS.contains(&l.as_str()));
                if !expletive && antecedents_before(tokens, idx) == 0 {
                    return true;
                }
            }
            _ => {}
        }
        if let Some((_, following)) = next {
            if DEICTIC_BIGRAMS.contains(&(lower.as_str(), following.as_str()))
                && antecedents_before(tokens, idx) == 0
            {
                return true;
            }
        }
    }
    false
}

/// Cosine similarity, `0.0` (never NaN) for mismatched, empty or all-zero vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

// ── Memory graph (causal DAG) ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRelation {
    /// This memory *led to* the creation of the target memory.
    Caused,
    /// This memory was *injected into context* when the target was created.
    Referenced,
    /// This memory *replaces* the target (e.g. consolidation, correction).
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEdge {
    pub from_id: String,
    pub to_id: String,
    pub relation: EdgeRelation,
    /// ISO-8601 timestamp when this edge was created.
    pub created_at: String,
}

/// A subgraph of the memory DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryGraph {
    pub nodes: Vec<MemoryFragment>,
    pub edges: Vec<MemoryEdge>,
}

// ── Memory audit log ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEventKind {
    Extracted,
    Written,
    Recalled,
    Archived,
    Pruned,
    Consolidated,
    Superseded,
    Deleted,
}

impl std::fmt::Display for MemoryEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Extracted => write!(f, "extracted"),
            Self::Written => write!(f, "written"),
            Self::Recalled => write!(f, "recalled"),
            Self::Archived => write!(f, "archived"),
            Self::Pruned => write!(f, "pruned"),
            Self::Consolidated => write!(f, "consolidated"),
            Self::Superseded => write!(f, "superseded"),
            Self::Deleted => write!(f, "deleted"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvent {
    pub id: i64,
    pub event_kind: MemoryEventKind,
    pub memory_id: String,
    pub session_id: Option<String>,
    pub data: Option<String>,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These pass every other `fact_defect` check, so only the echo guard stops them.
    #[test]
    fn whatever_the_prompt_demonstrates_is_refused_verbatim() {
        for example in EXTRACTION_EXAMPLE_FACTS {
            assert_eq!(
                fact_defect(example),
                Some(FactDefect::EchoedExample),
                "{example:?} is a demonstration the prompt shows the model, not a fact \
                 about this household"
            );
            // Case and padding must not get a copy through.
            let padded = format!("  {}  ", example.to_lowercase());
            assert_eq!(fact_defect(&padded), Some(FactDefect::EchoedExample));
        }
    }

    #[test]
    fn a_real_fact_that_resembles_the_old_example_still_passes() {
        for content in [
            "The user's mother Florence lives in Nakuru.",
            "The user's sister Florence lives in Kisumu.",
            "The user's mother is called Florence.",
            "The user's mother Florence lives in Kisumu.",
        ] {
            assert_eq!(
                fact_defect(content),
                None,
                "{content:?} is a real fact that merely resembles a demonstration"
            );
        }
    }

    #[test]
    fn names_user_separates_facts_about_the_user_from_everything_else() {
        // Real user facts — the wording the extraction prompt teaches.
        assert!(names_user("The user's location is Nairobi."));
        assert!(names_user("User prefers concise greetings"));
        assert!(names_user("The user's mother Florence lives in Kisumu."));
        assert!(names_user("The users' shared calendar is on Google."));

        // Facts about someone else.
        assert!(!names_user("William Ruto is a Kenyan politician."));
        assert!(!names_user("William Ruto is the leader of Kenya."));
        assert!(!names_user("AI assistant"));
        assert!(!names_user("I am a computer program designed to assist"));
        assert!(!names_user("Kirk Lazarus is an Armenian Australian artist"));
    }

    #[test]
    fn names_user_matches_the_possessive() {
        assert!(names_user("The user's home city is Nairobi."));
    }

    #[test]
    fn cosine_similarity_is_one_for_parallel_and_zero_for_orthogonal() {
        assert!((cosine_similarity(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_degrades_to_zero_instead_of_nan() {
        // NaN would poison every similarity sort.
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn segment_serde_round_trip() {
        let seg = MemorySegment::Correction;
        let json = serde_json::to_string(&seg).unwrap();
        assert_eq!(json, "\"correction\"");
        let back: MemorySegment = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MemorySegment::Correction);
    }

    #[test]
    fn tier_serde_round_trip() {
        let tier = MemoryTier::Permanent;
        let json = serde_json::to_string(&tier).unwrap();
        assert_eq!(json, "\"permanent\"");
        let back: MemoryTier = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MemoryTier::Permanent);
    }

    #[test]
    fn lifecycle_serde_round_trip() {
        let lc = MemoryLifecycle::Archived;
        let json = serde_json::to_string(&lc).unwrap();
        assert_eq!(json, "\"archived\"");
        let back: MemoryLifecycle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MemoryLifecycle::Archived);
    }

    #[test]
    fn fragment_backward_compat_deser() {
        let json = r#"{
            "id": "old-1",
            "profile_id": null,
            "session_id": null,
            "content": "User likes coffee",
            "source": "chat",
            "tags": [],
            "created_at": "2024-01-01T00:00:00Z"
        }"#;
        let frag: MemoryFragment = serde_json::from_str(json).unwrap();
        assert_eq!(frag.id, "old-1");
        assert!(frag.segment.is_none());
        assert!(frag.importance.is_none());
        assert_eq!(frag.access_count, 0);
        assert!(frag.lifecycle.is_none());
    }

    #[test]
    fn from_extraction_sets_defaults() {
        let frag = MemoryFragment::from_extraction(
            "ext-1".to_string(),
            None,
            "User's name is Jerry".to_string(),
            MemorySegment::Identity,
            0.85,
            None,
        );
        assert_eq!(frag.segment, Some(MemorySegment::Identity));
        assert_eq!(frag.importance, Some(0.85));
        assert_eq!(frag.tier, Some(MemoryTier::Permanent));
        assert_eq!(frag.decay_rate, Some(0.0));
        assert_eq!(frag.lifecycle, Some(MemoryLifecycle::Active));
        assert_eq!(frag.source, "extraction");
        assert!(frag.corrects.is_none());
    }

    #[test]
    fn from_extraction_with_corrects() {
        let frag = MemoryFragment::from_extraction(
            "corr-1".to_string(),
            None,
            "User's name is Jerry, not John".to_string(),
            MemorySegment::Correction,
            0.9,
            Some("User's name is John".to_string()),
        );
        assert_eq!(frag.segment, Some(MemorySegment::Correction));
        assert_eq!(frag.corrects, Some("User's name is John".to_string()));
    }

    #[test]
    fn segment_defaults() {
        assert_eq!(MemorySegment::Correction.default_importance(), 0.9);
        assert_eq!(MemorySegment::Context.default_importance(), 0.3);
        assert_eq!(
            MemorySegment::Identity.default_tier(),
            MemoryTier::Permanent
        );
        assert_eq!(MemorySegment::Context.default_tier(), MemoryTier::Short);
    }

    #[test]
    fn edge_relation_serde_round_trip() {
        let rel = EdgeRelation::Caused;
        let json = serde_json::to_string(&rel).unwrap();
        assert_eq!(json, "\"caused\"");
        let back: EdgeRelation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, EdgeRelation::Caused);
    }

    #[test]
    fn memory_edge_serde_round_trip() {
        let edge = MemoryEdge {
            from_id: "a".to_string(),
            to_id: "b".to_string(),
            relation: EdgeRelation::Referenced,
            created_at: "2025-01-01T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&edge).unwrap();
        let back: MemoryEdge = serde_json::from_str(&json).unwrap();
        assert_eq!(back, edge);
    }

    #[test]
    fn memory_graph_contains_nodes_and_edges() {
        let graph = MemoryGraph {
            nodes: vec![MemoryFragment::from_chat(
                "n1".to_string(),
                None,
                None,
                "test".to_string(),
            )],
            edges: vec![MemoryEdge {
                from_id: "n1".to_string(),
                to_id: "n2".to_string(),
                relation: EdgeRelation::Superseded,
                created_at: "2025-06-01T00:00:00Z".to_string(),
            }],
        };
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].relation, EdgeRelation::Superseded);
    }

    #[test]
    fn memory_event_kind_serde_round_trip() {
        let kind = MemoryEventKind::Extracted;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, "\"extracted\"");
        let back: MemoryEventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MemoryEventKind::Extracted);
    }

    #[test]
    fn memory_event_kind_display() {
        assert_eq!(MemoryEventKind::Written.to_string(), "written");
        assert_eq!(MemoryEventKind::Pruned.to_string(), "pruned");
        assert_eq!(MemoryEventKind::Superseded.to_string(), "superseded");
    }

    #[test]
    fn is_correction_by_segment() {
        let frag = MemoryFragment::from_extraction(
            "c1".to_string(),
            None,
            "Name is Jerry".to_string(),
            MemorySegment::Correction,
            0.9,
            None,
        );
        assert!(frag.is_correction());
    }

    #[test]
    fn is_correction_by_corrects_field() {
        let frag = MemoryFragment::from_extraction(
            "c2".to_string(),
            None,
            "Likes tea not coffee".to_string(),
            MemorySegment::Preference,
            0.7,
            Some("Likes coffee".to_string()),
        );
        assert_eq!(frag.segment, Some(MemorySegment::Preference));
        assert!(frag.is_correction());
    }

    #[test]
    fn keep_moves_to_the_permanent_tier_and_stops_decay() {
        let mut frag = MemoryFragment::from_extraction(
            "r1".to_string(),
            None,
            "Orders sushi on Fridays".to_string(),
            MemorySegment::Routine,
            0.65,
            None,
        );
        assert!(!frag.is_kept());

        frag.keep();

        assert!(frag.is_kept());
        assert_eq!(frag.decay_rate, Some(0.0));
    }

    #[test]
    fn a_paragraph_is_refused_as_more_than_one_fact() {
        let one = "Jerry Anyumba prefers the light to be set to a warm colour in the evening.";
        assert_eq!(fact_defect(one), None);
        let many = format!(
            "Jerry Anyumba is an engineer in Nairobi. {}",
            "He likes tea. ".repeat(25)
        );
        assert!(many.chars().count() > MAX_FACT_CONTENT_LEN);
        assert_eq!(fact_defect(&many), Some(FactDefect::TooLong));
        assert_eq!(FactDefect::TooLong.as_str(), "more than one fact");
    }

    #[test]
    fn is_correction_neither() {
        let frag = MemoryFragment::from_extraction(
            "k1".to_string(),
            None,
            "Works at Jarida".to_string(),
            MemorySegment::Identity,
            0.8,
            None,
        );
        assert!(!frag.is_correction());
    }

    // ── fact quality gate ───────────────────────────────────────────────

    /// Real content that must survive the gate; a wrongly rejected fact is gone for good.
    const KEEPERS: &[&str] = &[
        "The user's mother lives in Kisumu.",
        "The user's mother's name is Florence.",
        // "therapist" contains "there"; token matching must not see it.
        "The user's therapist is Dr. Amina.",
        "The user thereafter switched to decaf coffee.",
        // "mine" as a noun, "us" as a country, "I" as a numeral.
        "The user works in a mine near Kakamega.",
        "The user works in a coal mine near Kakamega.",
        "The user explored an abandoned mine on a school trip.",
        "The user's uncle owns a gold mine.",
        "The user lives in the US and visits Kenya when the rains break.",
        "The user has Type I diabetes.",
        // Expletive "there", not a place.
        "There is a spare key under the doormat.",
        "The user says there are two dogs in the compound.",
        // "the former" naming its referent; "the latter" with two candidates of any kind.
        "The user grew up in the former Yugoslavia.",
        "The user moved from Nairobi to Kisumu and prefers the latter.",
        "The user compared Rust and Go and prefers the latter.",
        // "there" with its place named earlier, via a preposition or a copula.
        "The user moved to Kisumu and still works there.",
        "The user's home town is Kisumu and his parents still live there.",
        "The user's employer is Jarida and the user works there full time.",
        // Words that merely contain a flagged token.
        "The user prefers the shorter route to work.",
        "The user is a formerly published poet.",
        "The user's houseplants are watered every evening.",
    ];

    const REJECTS: &[(&str, FactDefect)] = &[
        (
            "The user's mother lives in the latter city.",
            FactDefect::UnresolvedReference,
        ),
        // One earlier name is not enough for "the latter".
        (
            "The user's mother Florence lives in the latter city.",
            FactDefect::UnresolvedReference,
        ),
        ("I'm allergic to peanuts.", FactDefect::FirstPerson),
        (
            "I've been learning Swahili for two years.",
            FactDefect::FirstPerson,
        ),
        ("We're planning a trip to Mombasa.", FactDefect::FirstPerson),
        ("I\u{2019}m allergic to peanuts.", FactDefect::FirstPerson),
        (
            "My mom's name is Florence and she lives in the latter city",
            FactDefect::FirstPerson,
        ),
        (
            "The user's brother lives there.",
            FactDefect::UnresolvedReference,
        ),
        (
            "She lives in Kisumu and works as a nurse.",
            FactDefect::UnresolvedReference,
        ),
        ("Her name is Florence.", FactDefect::UnresolvedReference),
        ("It is broken again today.", FactDefect::UnresolvedReference),
        (
            "The user enjoyed that place a great deal.",
            FactDefect::UnresolvedReference,
        ),
        ("I prefer tea over coffee.", FactDefect::FirstPerson),
        (
            "The user asked me to water the plants.",
            FactDefect::FirstPerson,
        ),
        (
            "We are planning a trip to Mombasa.",
            FactDefect::FirstPerson,
        ),
        ("Our dog is called Rex.", FactDefect::FirstPerson),
        ("Tea.", FactDefect::TooShort),
        ("   ", FactDefect::TooShort),
        (
            "The user's dentist appointment is on Tuesday.",
            FactDefect::CalendarDate,
        ),
        (
            "The user moved to Kisumu in 2019.",
            FactDefect::CalendarDate,
        ),
        ("The current time is 09:54.", FactDefect::CalendarDate),
    ];

    #[test]
    fn well_formed_facts_pass_the_gate() {
        for content in KEEPERS {
            assert_eq!(
                fact_defect(content),
                None,
                "should have been kept: {content:?}"
            );
        }
    }

    #[test]
    fn defective_facts_are_rejected_with_the_right_reason() {
        for (content, expected) in REJECTS {
            assert_eq!(
                fact_defect(content),
                Some(*expected),
                "wrong verdict for {content:?}"
            );
        }
    }

    #[test]
    fn resolvable_and_dangling_latter_are_told_apart() {
        assert!(fact_defect("The user's mother lives in the latter city.").is_some());
        assert!(fact_defect(
            "The user's mother moved from Nairobi to Kisumu and lives in the latter city."
        )
        .is_none());
    }

    #[test]
    fn normalise_strips_an_invented_label_prefix() {
        assert_eq!(
            normalise_fact_content(
                "Active Project: Create a short Python function to check if a number is prime."
            ),
            "Create a short Python function to check if a number is prime."
        );
        assert_eq!(
            normalise_fact_content("Note:  the pump runs at dawn"),
            "the pump runs at dawn"
        );
    }

    #[test]
    fn normalise_leaves_a_sentence_that_merely_contains_a_colon() {
        assert_eq!(
            normalise_fact_content("The user's rule: keep replies short"),
            "The user's rule: keep replies short"
        );
        assert_eq!(
            normalise_fact_content("The   user  likes\ttea "),
            "The user likes tea"
        );
    }

    #[test]
    fn captured_requests_are_told_apart_from_real_projects() {
        assert!(is_captured_request(
            "Create a short Python function to check if a number is prime."
        ));
        assert!(is_captured_request(
            "Set a reminder to water the plants every evening at 6 PM"
        ));
        assert!(!is_captured_request(
            "The user is building a smart-home dashboard for the Jetson."
        ));
        assert!(!is_captured_request(
            "The user wants to write a book about beekeeping."
        ));
        assert!(!is_captured_request("Setup"));
    }

    #[test]
    fn an_imperative_opener_alone_does_not_demote_a_project() {
        for durable in [
            "Build a treehouse for the children this summer",
            "Write a novel about beekeeping",
            "Run the Nairobi marathon in October",
            "Design the new logo for Jarida",
            "Learn Swahili before the trip to Mombasa",
            // Assistant-sounding verbs that open durable projects.
            "Convert the garage into a workshop this year",
            "Install the solar panels on the roof before the rains",
        ] {
            assert!(!is_captured_request(durable), "demoted: {durable:?}");
        }
    }

    #[test]
    fn assistant_work_is_still_demoted() {
        // An imperative opener plus a named artifact in the object.
        for request in [
            "Create a short Python function to check if a number is prime.",
            "Set a reminder to water the plants every evening at 6 PM",
            "Write an email to the landlord about the leak",
            "Make a list of the groceries",
            "Schedule a meeting with the landlord for Monday",
            "Translate the summary into Swahili.",
        ] {
            assert!(is_captured_request(request), "not demoted: {request:?}");
        }
    }

    #[test]
    fn an_assistant_verb_without_an_artifact_is_left_alone() {
        // Accepted misses: they stay in Project until consolidation retires them.
        for missed in [
            "Translate the poem into Swahili.",
            "Explain how the decay formula works.",
            "Refactor the retrieval loop.",
        ] {
            assert!(!is_captured_request(missed), "demoted: {missed:?}");
        }
    }

    #[test]
    fn mine_is_no_longer_a_first_person_marker() {
        // Deliberate: a missed pronoun costs a vague row; a misread noun loses a true fact.
        for kept in [
            "The user works in a coal mine near Kakamega.",
            "The user explored a very old abandoned mine.",
            // Genuinely first person, stored anyway.
            "That laptop is mine now.",
            "A friend of mine works at Jarida.",
        ] {
            assert_eq!(fact_defect(kept), None, "rejected: {kept:?}");
        }
        // The unambiguous first-person markers still fire.
        assert_eq!(
            fact_defect("My laptop is broken again."),
            Some(FactDefect::FirstPerson)
        );
    }

    #[test]
    fn a_deictic_resolves_to_any_earlier_proper_noun() {
        assert!(fact_defect("The user's brother Peter enjoyed that place well enough.").is_none());
        assert!(
            fact_defect("The user's home town is Kisumu and his parents still live there.")
                .is_none()
        );
        // With nothing named before it, the deictic is still dangling.
        assert!(fact_defect("The user's brother enjoyed that place well enough.").is_some());
        assert!(fact_defect("The user's brother lives there.").is_some());
    }

    // ── Dates ────────────────────────────────────────────────────────────

    #[test]
    fn the_dated_shapes_the_models_actually_write_are_refused() {
        for note in [
            // The appointment class: every model leaks it.
            "The user has a dentist appointment next Tuesday.",
            "The user has a dentist appointment on Tuesday.",
            "The user's lease ends in May.",
            "The user's passport expires in November 2027.",
            "The user's passport expires on 3 November 2027.",
            // Biographical and version years, and the decade built on one.
            "The user moved to Kisumu in 2019.",
            "The user grew up in Kisumu in the 1990s.",
            // One token, so no word inside it is ever compared with anything.
            "The user's passport expires 2027-11-03.",
            "The user's daughter Amara was born on 14/03/1984.",
            // Clock times, spelled and written.
            "The user's standup is at six.",
            "The user runs the Jarida standup at six pm.",
            "The user takes his blood pressure pills at 9am.",
            "The user's appointment is at 09:00.",
            // Days of the month, spelled and numbered.
            "The user's lease ends on the fourteenth.",
            "The user's anniversary is the third of May.",
            "The user's rent is due on the 1st of every month.",
            // A stretch of calendar time is a date said relatively.
            "The user is repainting the kitchen in three weeks.",
            "The user is travelling to Mombasa next week.",
            "The user is travelling to Mombasa tomorrow.",
        ] {
            assert!(
                carries_calendar_date(note),
                "{note:?} carries a date the models were measured to write, and the \
                 detector does not see it"
            );
            assert_eq!(
                fact_defect(note),
                Some(FactDefect::CalendarDate),
                "{note:?} must be refused for the DATE, not for anything else"
            );
        }
    }

    #[test]
    fn a_recurrence_is_a_habit_and_is_never_refused() {
        for note in [
            "The user swims at the club each Saturday morning.",
            "The user swims at the club on Saturdays.",
            "The user cooks ugali each Friday night.",
            "The user walks the dog every Sunday evening.",
            "The user's household eats no meat on weekdays.",
            "The user works at the weekend.",
            "The user plants maize in the long rains each March and October.",
            "The user is at the workshop from Monday to Friday.",
            // The clock time belongs to the habit too.
            "The user runs the Jarida standup every Monday at 09:00.",
            "The user drinks chai at six each morning.",
        ] {
            assert!(
                !carries_calendar_date(note),
                "{note:?} is a habit, not a date. Refusing it loses the pattern the \
                 extractor exists to find, and no reminder is filed for a habit."
            );
            assert_eq!(
                fact_defect(note),
                None,
                "{note:?} is a habit and the gate refused it"
            );
        }
    }

    /// Numbers and names a household writes that are NOT dates.
    #[test]
    fn the_not_dates_the_stripper_kept_eating() {
        for note in [
            // A bare number after "at" is not a clock hour.
            "The user keeps the oven at 180.",
            "The user sets the thermostat at 21.",
            "The user keeps the tyres at 40 psi.",
            // A cat's name, not a time.
            "The user's cat is called Midnight.",
            // The modal, which a word list reads as the month of May.
            "The user may travel to Kisumu.",
            // Birth order, not a day of the month.
            "The user's daughter Amara is the second of four.",
            // Two numbers and a slash, with no year in them.
            "The user's blood pressure runs 120/80.",
            // A dot with two parts is how software is numbered.
            "The user's greenhouse controller runs build 2019.1 of the firmware.",
            // A count is not an hour.
            "The user keeps six chickens.",
            // "first thing" is not a day of the month.
            "The user waters the greenhouse beds first thing every morning.",
        ] {
            assert!(
                !carries_calendar_date(note),
                "{note:?} carries no date, and refusing it now throws the fact away"
            );
            assert_eq!(
                fact_defect(note),
                None,
                "{note:?} carries no date and the gate refused it anyway"
            );
        }
    }

    /// Deliberate over-fire: a year or digit ordinal that isn't a date looks exactly like one.
    #[test]
    fn the_year_that_is_a_name_is_refused_with_the_year_that_is_a_date() {
        for note in [
            "The user prefers the 2019 model of the tractor.",
            "The user still uses the 1998 recipe book.",
            "The user's flat is on the 4th floor.",
            "The user's daughter Amara finished 2nd in the county exam.",
        ] {
            assert_eq!(
                fact_defect(note),
                Some(FactDefect::CalendarDate),
                "{note:?} is the known over-fire, and it must be a REFUSAL -- the one \
                 thing it must never become again is a rewrite"
            );
        }
    }

    #[test]
    fn the_gate_returns_a_verdict_and_never_a_sentence() {
        let note = "The user runs the Jarida standup every Monday at 09:00.";
        assert_eq!(normalise_fact_content(note), note);
        assert_eq!(fact_defect(note), None);
        // And the refusal is total: there is no partial form of a dated note.
        let dated = "The user has a dentist appointment next Tuesday at 09:00.";
        assert_eq!(fact_defect(dated), Some(FactDefect::CalendarDate));
    }

    /// An undated sentence is not made suspicious by containing numbers.
    #[test]
    fn an_undated_sentence_passes_untouched() {
        for note in [
            "The user's mother Florence lives in Kisumu.",
            "The user prefers short answers with no preamble.",
            "The pond runs on a Jetson Orin Nano with 8 GB of RAM.",
            "The user's flat is 1200 square feet.",
        ] {
            assert!(!carries_calendar_date(note));
            assert_eq!(fact_defect(note), None);
        }
    }

    // ── Does a reminder cover this note ─────────────────────────────────

    fn jerry() -> Vec<String> {
        vec!["Jerry".to_string()]
    }

    #[test]
    fn a_reminder_covers_the_note_it_shares_its_subject_with() {
        assert!(reminder_covers_note(
            "the dentist",
            "Jerry has a dentist appointment next Tuesday.",
            &jerry()
        ));
    }

    #[test]
    fn a_reminder_about_something_else_covers_nothing() {
        assert!(!reminder_covers_note(
            "the dentist",
            "Jerry is collecting the tractor on 3 March.",
            &jerry()
        ));
    }

    #[test]
    fn a_shared_date_alone_is_not_a_match() {
        assert!(!reminder_covers_note(
            "the dentist on Tuesday",
            "Jerry sees the farrier next Tuesday.",
            &jerry()
        ));
    }

    #[test]
    fn the_subjects_own_name_is_not_a_match() {
        assert!(!reminder_covers_note(
            "Jerry",
            "Jerry is collecting the tractor on 3 March.",
            &jerry()
        ));
    }

    #[test]
    fn a_plural_still_matches_its_singular() {
        assert!(reminder_covers_note(
            "collect the tractors",
            "Jerry is collecting the tractor on 3 March.",
            &jerry()
        ));
    }

    #[test]
    fn a_paraphrase_is_reported_as_uncovered() {
        // A known limit, pinned on purpose.
        assert!(!reminder_covers_note(
            "the surgery",
            "Jerry has a dentist appointment next Tuesday.",
            &jerry()
        ));
    }
}
