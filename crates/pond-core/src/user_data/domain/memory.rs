//! Memory fragment domain type — stores snippets of conversation or facts
//! with optional embedding vectors for semantic search.
//!
//! Memories are categorised by [`MemorySegment`] (identity, preference, etc.),
//! assigned a [`MemoryTier`] that controls decay, and tracked with importance
//! scoring and access counts for intelligent cleanup.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

// ── Memory classification ────────────────────────────────────────────────────

/// Semantic category of a memory (inspired by boop-agent segments).
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
    /// Something the user does again and again: a habit, a standing way they
    /// do things.
    ///
    /// The variant the batch engine exists for. "Is this a one-off, or a
    /// pattern?" is the third of what a household wants remembered, and it is
    /// the question a per-turn extractor cannot be asked — a habit is not
    /// visible inside one exchange. No migration: `memories.segment` is bare
    /// TEXT with no CHECK constraint, so the new label round-trips through
    /// serde the day the variant exists.
    Routine,
    /// Factual knowledge worth remembering.
    Knowledge,
    /// Transient context (current situation, ongoing state).
    Context,
}

impl MemorySegment {
    /// Default importance for this segment (0.0–1.0).
    pub fn default_importance(&self) -> f32 {
        match self {
            Self::Correction => 0.9,
            Self::Identity => 0.8,
            Self::Preference => 0.7,
            Self::Relationship => 0.7,
            // Between a preference and a project: a habit is more durable than
            // a piece of work, and less definitive than a stated preference,
            // because it is inferred from what somebody did rather than from
            // what they said they wanted.
            Self::Routine => 0.65,
            Self::Project => 0.6,
            Self::Knowledge => 0.5,
            Self::Context => 0.3,
        }
    }

    /// Default tier for this segment.
    pub fn default_tier(&self) -> MemoryTier {
        match self {
            Self::Identity => MemoryTier::Permanent,
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

/// Lifecycle status for memory management.
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

/// A persisted memory fragment.
///
/// `embedding` is stored as a raw f32 BLOB in SQLite and is `#[serde(skip)]`
/// so it never appears in JSON responses (it's binary data, not user-facing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFragment {
    pub id: String,
    /// Profile this memory belongs to (None = global)
    pub profile_id: Option<String>,
    /// Session this memory was extracted from (None = manual/external)
    pub session_id: Option<String>,
    /// The text content of the memory
    pub content: String,
    /// Raw embedding vector (None until an EmbeddingProvider generates it)
    #[serde(skip)]
    pub embedding: Option<Vec<f32>>,
    /// Source of this fragment: "chat", "note", "sensor_summary", "extraction", "mcp_tool"
    pub source: String,
    /// Optional tags for categorization
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,

    // ── Segment-aware fields (all optional for backward compat) ───────────
    /// Semantic category of this memory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment: Option<MemorySegment>,
    /// Importance score (0.0–1.0). Higher = more worth retaining.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importance: Option<f32>,
    /// Decay tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<MemoryTier>,
    /// Decay rate (lambda). Defaults from tier if absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decay_rate: Option<f32>,
    /// Number of times this memory has been accessed (recalled or injected).
    #[serde(default)]
    pub access_count: u32,
    /// When the memory was last accessed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_accessed_at: Option<DateTime<Utc>>,
    /// Lifecycle status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<MemoryLifecycle>,
    /// ID of the memory that superseded this one (via consolidation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// For correction memories: describes what wrong claim this corrects,
    /// so consolidation never accidentally reverts the fix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrects: Option<String>,
}

impl MemoryFragment {
    /// Create a fragment sourced from a chat exchange.
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
        }
    }

    /// True if this memory represents a user correction — either by segment
    /// classification or by having a `corrects` field set.
    ///
    /// Correction memories must never be pruned or merged away during
    /// consolidation, as they represent explicit user fixes.
    pub fn is_correction(&self) -> bool {
        self.segment.as_ref() == Some(&MemorySegment::Correction) || self.corrects.is_some()
    }

    /// Create a fragment from background memory extraction.
    ///
    /// `corrects` should be set for `Correction` segments to record
    /// what wrong claim this memory fixes, preventing consolidation
    /// from accidentally reverting the correction.
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
        }
    }

    /// Create a fragment from one window of batch extraction.
    ///
    /// Separate from [`from_extraction`](Self::from_extraction) rather than a
    /// widening of it, because the two disagree about the tier and only one of
    /// them can be right. `from_extraction` derives `tier` from the segment,
    /// and `Identity::default_tier()` is `Permanent` — so a `context` memory
    /// ("who the subject is: role, home, the work they are living through")
    /// would be filed as something that never decays and is never pruned. The
    /// batch catalogue says `Long` for all five kinds: a household's
    /// circumstances change, and a permanent row asserting a job somebody left
    /// is worse than one that fades.
    ///
    /// Changing `from_extraction` instead would have moved the tier under
    /// ~20 existing fixtures and under every row the MCP `save_memory` tool
    /// writes. The `source` string is the same `"extraction"`, so the desktop's
    /// provenance badge keeps working.
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
        }
    }
}

// ── Fact quality gate ───────────────────────────────────────────────────────

/// Shortest trimmed content, in characters, that can carry a fact.
pub const MIN_FACT_CONTENT_LEN: usize = 8;

/// Why a candidate memory was refused at write time.
///
/// A stored memory is injected into the assistant's context on later turns,
/// long after the conversation that produced it is gone. Anything that only
/// made sense inside that conversation is not merely useless — it actively
/// misleads — so it is cheaper to lose the fact than to keep it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactDefect {
    /// Nothing left after normalisation, or too short to carry a fact.
    TooShort,
    /// Contains a deictic with no antecedent inside the sentence — "the latter
    /// city", a trailing "there", a leading bare pronoun.
    UnresolvedReference,
    /// Written from the user's point of view ("my mother"). Injected into the
    /// assistant's context, "my" reads as the *assistant's* mother.
    FirstPerson,
    /// A verbatim copy of the extraction prompt's own worked example.
    ///
    /// `EXTRACTION_PROMPT` teaches the JSON shape with a demonstration — "my
    /// mom florence lives in kisumu" mapping to two facts about Florence and
    /// Kisumu. A model at this size copies worked examples: the answer
    /// contract's Nairobi example was emitted verbatim as a real answer by a 4B
    /// model on 2026-08-25, and the same class of failure here writes invented
    /// family facts into the user's memory store, permanently and silently.
    ///
    /// Nothing else catches it. The example's facts are third person,
    /// well-formed, self-contained and long enough — they pass every other
    /// check in `fact_defect`, because they were written to.
    ///
    /// So the demonstration stays (it is what carries format compliance at this
    /// size) and its own output is refused deterministically. A guard the model
    /// cannot argue with is the only kind worth having against a model copying
    /// text.
    EchoedExample,
    /// The note carries a calendar date, so the whole note is refused.
    ///
    /// Not a last resort any more: this IS the date rule. Nothing rewrites a
    /// note to take a date out of it — four passes at that stored "The user
    /// swims morning.", "The user prefers model of the tractor." and "The user
    /// keeps the oven." — so a dated note is refused whole and the model is the
    /// only thing that decides what a memory says.
    ///
    /// The trade is deliberate and asymmetric. A memory is read back six months
    /// later with no conversation around it, and "the dentist is on Tuesday" is
    /// by then not merely useless but false. A refused fact is still in the
    /// conversation and can be extracted again; a mangled one is shown to the
    /// household forever. Anything whose point was the date belongs in a
    /// reminder, which expires — and the prompt asks the model to file one,
    /// which the shipped model does for 31 of its 36 dated windows, losing no
    /// date entirely across 72.
    CalendarDate,
    /// The sentence stops on a word that was leading into something else.
    ///
    /// "The user's anniversary is", "The user's cat is called". Long enough,
    /// third person, self-contained, and saying nothing whatever.
    ///
    /// This rung was written to catch what the date STRIPPER left behind, and
    /// the stripper is gone. It stays because a model truncates its own reply
    /// too — one of 432 measured replies stopped mid-sentence on a token limit
    /// — and because it costs one word-list lookup on the last token. It no
    /// longer fires on anything this pond generates itself.
    Fragment,
}

impl FactDefect {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TooShort => "too short",
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

/// Label prefixes a small model likes to bolt onto fact content
/// ("Active Project: …"). Matched case-insensitively against the text before
/// an early colon.
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

/// Verbs that can open a *captured request* — a copy of what the user asked
/// for this turn rather than a statement about the user. A bare imperative
/// opener is necessary but nowhere near sufficient: "Build a treehouse for the
/// children this summer" and "Run the Nairobi marathon in October" open the
/// same way and are durable undertakings. See [`is_captured_request`].
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

/// Objects that mark an imperative as work the assistant does and finishes.
/// Deliberately concrete: a "reminder" or a "function" is produced and done
/// with, a "treehouse", "novel", "marathon" or "logo" is not.
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

/// First-person markers that are unambiguous wherever they appear.
///
/// "i" and "us" are handled separately — each collides with a real word.
/// "mine" is deliberately absent: it is a common noun ("a coal mine") far more
/// often than a predicate pronoun ("that laptop is mine"), and every rule that
/// tried to tell the two apart produced new false positives in one direction or
/// the other. Storing "That laptop is mine now." is the cheaper error.
const FIRST_PERSON: &[&str] = &["my", "myself", "our", "ours", "ourselves", "we", "me"];

/// Contracted first-person forms. [`split_tokens`] keeps internal apostrophes
/// so "I'm" stays one token and never reaches the bare-pronoun arms below —
/// "I'm allergic to peanuts." was being stored verbatim.
const FIRST_PERSON_CONTRACTIONS: &[&str] = &[
    "i'm", "i've", "i'll", "i'd", "we're", "we've", "we'll", "we'd", "let's",
];

/// Tokens that make a preceding "I" a pronoun subject rather than a numeral or
/// an initial ("Type I diabetes" must survive).
const I_PREDICATES: &[&str] = &[
    "am", "was", "have", "had", "will", "would", "can", "could", "should", "do", "did", "like",
    "prefer", "want", "need", "think", "live", "work", "use", "enjoy", "hate", "love", "also",
    "just", "usually", "always", "never", "often",
];

/// How many in-sentence antecedents "the latter" / "the former" need. They
/// *select between two* candidates, so one proper noun is not enough — "The
/// user's mother Florence lives in the latter city" still names no city.
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

/// Verbs that make a leading "there" the expletive subject ("there is a leak")
/// rather than a place the reader cannot find.
const EXPLETIVE_FOLLOWERS: &[&str] = &[
    "is", "are", "was", "were", "will", "would", "has", "have", "had", "seems", "appears",
];

/// Split into (raw, lowercased) tokens with edge punctuation removed.
///
/// Edge-only trimming keeps internal apostrophes and hyphens, so "user's" stays
/// one token and "U.S." never collapses onto the pronoun "us".
fn split_tokens(content: &str) -> Vec<(&str, String)> {
    content
        .split_whitespace()
        .map(|raw| raw.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|t| !t.is_empty())
        .map(|t| (t, t.to_lowercase()))
        .collect()
}

/// Clean up fact content before it is validated or stored: collapse whitespace
/// and drop a leading segment label the model invented.
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

/// Inspect fact content and return the first defect that makes it unstorable.
///
/// Deliberately conservative: every rule here permanently discards a fact, so
/// each one is anchored to a token pattern that a well-formed third-person
/// sentence cannot produce.
pub fn fact_defect(content: &str) -> Option<FactDefect> {
    let trimmed = content.trim();
    if trimmed.chars().count() < MIN_FACT_CONTENT_LEN {
        return Some(FactDefect::TooShort);
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
    // The date rule, in the one place that decides whether a fact is storable.
    // It used to run BEFORE this function as a rewrite, and this rung was what
    // caught whatever the rewrite missed; now there is no rewrite and this is
    // the whole of it. See the long note above [`carries_calendar_date`].
    if carries_calendar_date(trimmed) {
        return Some(FactDefect::CalendarDate);
    }
    // After the date rung, so a dated sentence is refused for the date rather
    // than for how it ends. What this catches now is a model that stopped
    // mid-sentence -- measured once in 432 replies, on a token limit.
    if tokens.last().is_some_and(|(_, lower)| {
        DANGLING_TAIL_WORDS.contains(&lower.as_str())
            || DANGLING_TAIL_VERBS.contains(&lower.as_str())
    }) {
        return Some(FactDefect::Fragment);
    }
    None
}

/// Words a finished sentence does not end on.
///
/// Copulas, prepositions, conjunctions and determiners — every one of them
/// announces something that is not there. Kept deliberately short: every entry
/// permanently discards a fact, and the only thing it has to catch is a reply
/// that stopped before its own last word.
const DANGLING_TAIL_WORDS: &[&str] = &[
    "is", "are", "was", "were", "be", "been", "being", "am", "and", "or", "but", "of", "on", "in",
    "at", "to", "by", "with", "for", "from", "into", "the", "a", "an", "every", "each", "about",
    "than", "as", "that", "which", "who", "until", "till", "since", "during", "within", "around",
    "before", "after",
];

/// The same argument, one part of speech over: verbs a clause does not end on.
///
/// "The user's cat is called." is long enough, third person, self-contained,
/// very nearly a sentence, and says nothing at all. Each of these exists only
/// to introduce the word that is missing, so a clause ending on one has lost
/// its content.
///
/// The sentence above is not hypothetical: it is what the date stripper stored
/// when it read the cat's name, Midnight, as a time. The stripper is gone and
/// the cat's name is kept whole now — `carries_calendar_date` does not treat
/// "midnight" as a date at all — but a truncated reply still ends this way.
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
//
// # Why there is no stripper here any more
//
// There used to be one. It read a note the model had written, decided which
// words in it were a date, and rewrote the sentence without them. It was
// widened four times, and each pass fixed its own list of probes and left a new
// class of wreckage behind:
//
//   pass 1  "swims each Saturday morning"           -> "The user swims morning."
//   pass 2  "prefers the 2019 model of the tractor" -> "The user prefers model of the tractor."
//   pass 3  "keeps the oven at 180"                 -> "The user keeps the oven." + a reminder for 180
//   pass 4  a damage gate that refused the mangles -- and refused 19 of 19
//           clean strips along with them, because it could not tell a clean
//           result from wreckage either
//
// That is not a bug that was four fixes away. Deciding which words in a
// sentence are a date, and whether the sentence still says what it said once
// they are gone, is a judgement about MEANING. A word list over whitespace
// tokens cannot make it: "2019" is a date in "moved to Kisumu in 2019" and a
// model number in "the 2019 model of the tractor", and nothing in the token
// stream separates them. The thing that CAN separate them wrote the sentence.
//
// So the rewriting is gone and this is a DETECTOR: it answers yes or no, and a
// note it says yes to is refused rather than repaired. A refused fact is still
// in the conversation and can be extracted again; a repaired one is read back
// to the household forever with a word missing from the middle of it.
//
// Two consequences worth stating, because they are what makes the detector
// cheap enough to be safe:
//
//  - It only has to be right about whole sentences, never about spans. No
//    contiguity walk, no lead-popping, no damage gate -- those existed to
//    decide where an edit began and ended, and there is no edit.
//  - A false positive now costs a good fact, so the rules are narrower than the
//    stripper's were and fire only on shapes the local models were MEASURED to
//    leak (432 live replies, six models): weekdays and months for an
//    appointment, biographical and version years, clock times, relative days,
//    numbered days of the month, counted stretches of time. A bare integer
//    after "at" is NOT one of them -- that was the oven, and the six models
//    return that sentence clean 36 times of 36 -- and neither is "midnight",
//    which is a cat in this household, nor a modal "may".
//
// Two classes are still over-fired on, knowingly, and they are the boundary
// this design draws rather than an edge nobody thought about. A year used as a
// NAME -- "the 2019 model of the tractor" -- is refused alongside a year used
// as a date, because `(19|20)\d\d` is all either of them looks like from here.
// A digit ordinal naming a floor -- "on the 4th floor" -- is refused alongside
// a day of the month, for the same reason. Each loses one true fact that is
// still sitting in the conversation; storing either keeps something that reads
// as a date for as long as the pond runs. The model is the only thing that
// could draw those two lines, which is why the prompt asks it to.

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

/// "may" is a month and it is also a modal, and the modal is the commoner
/// reading by far in a household's memories. It counts as a date only where a
/// preposition or a day number has already fixed it as one — "in May", "3 May"
/// — and never on its own, because "The user may travel to Kisumu" is a fact
/// this pond refused for months and should not have.
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

/// Named stretches of the week, which are NOT days and never a date on their
/// own: "The user works at the weekend" names nothing that can go stale.
///
/// They are here for the recurrence rules only, where their plural is a habit
/// that marks itself -- "eats no meat on weekdays" -- and for "next weekend",
/// which the relative-head rule reaches on its own.
const PERIOD_WORDS: &[&str] = &["weekday", "weekend"];

/// Weekday abbreviations, kept OUT of [`WEEKDAY_WORDS`] on purpose.
///
/// "sat" is also a verb, "mar" is also a month abbreviation, "sun" is also a
/// noun a household says every day. They are only consulted where something
/// else has already fixed the reading: after "next" or "this", and inside the
/// `from X to Y` recurrence frame, where the token on the other side of "to"
/// is the disambiguator.
const WEEKDAY_ABBREVIATIONS: &[&str] = &[
    "mon", "tue", "tues", "wed", "weds", "thu", "thur", "thurs", "fri", "sat", "sun",
];

/// Single-word relative days.
///
/// "midnight" and "noon" are deliberately absent. Neither was ever measured
/// leaking out of a model, both are ordinary English nouns, and one of them is
/// a cat in this household — "The user's cat is called Midnight" is the exact
/// fact the old relative-time list destroyed.
const RELATIVE_WORDS: &[&str] = &["today", "tomorrow", "yesterday", "tonight", "overmorrow"];

/// Words that turn a preceding "next", "last" or "this" into a date.
const RELATIVE_HEADS: &[&str] = &["next", "last", "this", "coming", "past"];
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

/// Spelled days of the month.
///
/// Never consulted on its own: "first" is a date in "on the first" and is not
/// one in "waters the beds first thing every morning", and "the second of four"
/// is birth order. What decides it is the words either side.
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

/// Spelled clock hours. A date only after a [`CLOCK_LEADS`] word: "at six" is a
/// time, "has six chickens" is a count.
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

/// What leads into a counted stretch of time: "in three weeks" is a date,
/// "works in three offices" is not.
const DURATION_LEADS: &[&str] = &["in", "within"];

// ── Recurrence ──────────────────────────────────────────────────────────────

/// Words that turn a named weekday, month or clock time into a pattern rather
/// than a point on a calendar.
const RECURRENCE_MARKERS: &[&str] = &["each", "every", "daily", "nightly", "weekly", "monthly"];

/// What joins one item of a recurrence to the next: "each March and October",
/// "from Monday to Friday". Only consulted immediately after a token already
/// found to recur, so an ordinary "and" never drags a weekday into one.
const RECURRENCE_CONNECTIVES: &[&str] = &["and", "or", "to", "through", "thru"];

/// Whether this token names a weekday, a month, or a named stretch of the week.
///
/// The recurrence rules' notion of a calendar name, which is wider than the
/// date rule's: "each weekend" is a pattern, and "the weekend" is not a date.
fn is_calendar_name(lower: &str) -> bool {
    WEEKDAY_WORDS.contains(&lower) || MONTH_WORDS.contains(&lower) || PERIOD_WORDS.contains(&lower)
}

/// Whether this token names one day of the week or one month — the two that
/// DO name a point on a calendar when nothing marks them as recurring.
fn is_day_or_month_name(lower: &str) -> bool {
    WEEKDAY_WORDS.contains(&lower) || MONTH_WORDS.contains(&lower)
}

/// The same, plus the abbreviations — only where the caller has already fixed
/// the reading. See [`WEEKDAY_ABBREVIATIONS`].
fn is_calendar_name_or_abbrev(lower: &str) -> bool {
    is_calendar_name(lower) || WEEKDAY_ABBREVIATIONS.contains(&lower)
}

/// A weekday or month written plural: "Saturdays", "weekdays".
///
/// A plural weekday cannot name one day. It is a recurrence by its own grammar,
/// which is why it needs no marker in front of it.
fn is_plural_calendar_name(lower: &str) -> bool {
    lower
        .strip_suffix('s')
        .is_some_and(|stem| stem.len() > 3 && is_calendar_name(stem))
}

/// Which token positions name a weekday or month that RECURS.
///
/// This is the distinction that survives the stripper, because it is not about
/// editing at all: it is the line between a habit and an appointment. "The user
/// swims each Saturday morning" names no day on any calendar — there is nothing
/// to put in a diary and nothing in it can become false — and it is precisely
/// what the `routine` kind exists to capture. "The user has a dentist
/// appointment next Tuesday" names one day, is unrecoverable once the
/// conversation is gone, and is wrong by the following week.
///
/// The models will not drop these. The shipped model and the larger one kept
/// the weekday in all 12 recurring windows between them -- 6 of 6 each, at
/// greedy and at both seeds -- and granite did the same. A detector that called
/// those a date would refuse every one of them and delete the habit, so the
/// prompt asks for the timing to stay and this is where the gate agrees.
///
/// Four shapes: marked ("each Saturday"), plural ("on Saturdays"), a span
/// ("from Monday to Friday"), and a continuation ("each March and October").
///
/// NOT a recurrence: a numbered day of the month, even a repeating one ("the
/// 1st of every month"). That is the day a reminder is set for, and the number
/// is the whole content of it.
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
        // "from Monday to Friday", "Mon to Fri". The frame is what makes an
        // abbreviation readable, so both sides are checked before either is
        // accepted.
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

/// Whether the sentence describes something that happens again and again.
///
/// Used only to spare a CLOCK TIME inside a habit: "drinks chai at six every
/// morning" is a routine whose six o'clock is part of the pattern, not an
/// appointment, and the shipped model leaks exactly this shape on 3 windows of
/// 3. A specific date in the same sentence is still a date — nothing about
/// "every" makes "3 November 2027" repeat.
fn is_habitual(words: &[String], recurring: &[bool]) -> bool {
    recurring.iter().any(|r| *r)
        || words
            .iter()
            .any(|w| RECURRENCE_MARKERS.contains(&w.as_str()))
}

/// Whether a note carries a calendar date, and therefore cannot be stored.
///
/// The whole date rule, in one answer. Nothing edits the sentence: the caller
/// refuses it or keeps it exactly as the model wrote it.
///
/// Conservative by construction — every `true` here permanently discards a fact
/// the model thought was worth remembering — so each rule below is anchored to
/// a shape that was measured coming out of a local model with a date in it, and
/// the ambiguous readings a household actually produces (a temperature after
/// "at", a floor number, a version year, birth order, a cat called Midnight)
/// are left alone on purpose. Each of those has a fixture in
/// `memory_reachability_corpus` naming it.
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
        // Whether this token ends its clause: "on the fourteenth." is a date,
        // "the fourteenth row" modifies a noun.
        let closes = raw[i].ends_with(['.', ',', ';', ':', '!', '?']) || i + 1 >= raw.len();

        // A weekday or month that does not recur. The appointment class, and
        // the one every model leaks.
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
        // A year, a decade, an ISO or slash date. The one place the detector
        // knowingly over-fires: a year used as a name is refused with the rest.
        if is_year_like(w) || is_numeric_date_group(w) {
            return true;
        }
        // A numbered day of the month: "the 1st", "on the 14th", and "3
        // November" where only the month says what the number is.
        if is_numeric_ordinal(w) || (is_day_number(w) && next_in(MONTH_WORDS)) {
            return true;
        }
        // A spelled day of the month: "on the fourteenth", "the third of May".
        // Gated on both sides, because the bare word is far commoner as an
        // ordinary ordinal — "the second of four" is birth order.
        if ORDINAL_WORDS.contains(&w)
            && (prev_in(DATE_PREPOSITIONS) || prev == Some("the"))
            && (closes || (next == Some("of") && next2.is_some_and(|n| MONTH_WORDS.contains(&n))))
        {
            return true;
        }
        // "in three weeks", "within 10 days" — a stretch of calendar time,
        // which is a date said relatively.
        if (HOUR_WORDS.contains(&w) || is_all_digits(w))
            && prev_in(DURATION_LEADS)
            && next_in(DURATION_UNITS)
        {
            return true;
        }
        // Clock times, last, and skipped entirely inside a habit.
        //
        // A bare integer is NEVER one of these. "The user keeps the oven at
        // 180" was read as six past midnight by the rule this replaces, and the
        // shipped models return that sentence clean on 36 windows of 36. A
        // clock time here has to wear the morphology of one: a colon, a
        // meridiem, an o'clock, or a spelled hour.
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

/// One token, lowercased, with the punctuation around it taken off.
///
/// Edge-only trimming, so the separators inside a date survive: "2027-11-03."
/// loses its full stop and keeps its hyphens, and "09:00" keeps its colon.
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

/// A date written as numbers with separators: `2027-11-03`, `14/03/1984`,
/// `03.11.2027`, `03/11/27`.
///
/// The class a whitespace tokeniser cannot otherwise reach — the whole date is
/// one token. Narrow in the ambiguous direction: two numbers joined by a slash
/// are a blood pressure or a score as often as a date, so a group counts only
/// when it carries a four-digit year, or has three parts joined by a slash or a
/// hyphen. `2019.1` is a version string and is left alone; a dot-separated date
/// has all three parts.
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

/// An ordinal written with digits: "3rd", "21st", "4th".
///
/// A floor and a placing in an exam are written this way too, and both are
/// refused with the days of the month. That is the second place the detector
/// knowingly over-fires, and it is the cheaper direction: "on the 4th" is a
/// date in every household that has ever written it down.
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

/// The facts the extraction prompt's own worked example produces.
///
/// Compared case-insensitively and ignoring surrounding whitespace, not by
/// fuzzy similarity: a real user really might have a mother called Florence,
/// and refusing every fact that merely resembles the example would silently
/// lose true memories. Only a VERBATIM echo is refused, which is what a copying
/// model produces.
///
/// Kept next to `fact_defect` rather than in the extractor because it is a
/// property of a fact, and both the LLM extractor and any future one have to
/// answer to it.
///
/// **Empty, and that is the invariant.** The prompt that carried the
/// Florence-in-Kisumu demonstration is gone: the batch prompt shows a schema
/// skeleton with `...` for every value and no worked example at all, precisely
/// because a model at this size copies content it is shown. There is therefore
/// no example output to refuse.
///
/// This list and the prompt are coupled in both directions, and
/// `conversation_extractor`'s `the_prompt_and_the_echo_gate_agree_about_examples`
/// asserts it from the side that can see both: every sentence here must appear
/// in the prompt, and a prompt that grows a worked example must add its output
/// here. An entry with no demonstration behind it silently refuses a true
/// memory; a demonstration with no entry is how invented family facts reached
/// the live store on 2026-08-25.
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

/// True when the content is a copy of a one-off request the assistant already
/// carried out ("Set a reminder to water the plants") rather than a durable
/// undertaking of the user's.
///
/// A bare imperative opener is *grammar*, not transience: "Build a treehouse
/// for the children this summer" and "Design the new logo for Jarida" open the
/// same way and are exactly the long-lived projects this must not demote. So
/// two signals are required — an imperative opener **and** a named assistant
/// artifact in the object.
///
/// The verb alone is never enough, however assistant-ish it sounds: "Convert
/// the garage into a workshop this year" and "Install the solar panels on the
/// roof before the rains" are year-long undertakings that open on "convert" and
/// "install". The object is what separates work that gets produced and finished
/// from work the user lives with.
///
/// The rule is calibrated to under-demote. Missing a captured request leaves a
/// stale `Project` row that consolidation can retire; demoting a real project
/// drops it to `Short` tier and it decays away in about a week.
/// Whether a fact actually names the user.
///
/// The write gate tests the FORM of a sentence — length, no first person, no
/// dangling anaphor — and never its subject, so a well-formed sentence about
/// somebody else passes cleanly. On the device that let five William Ruto
/// biography facts land in the `identity` segment, where identity means "the
/// user's own name, role, home city", and one in `relationship`. 14 of 24
/// stored rows contained no reference to the user at all.
///
/// Deliberately a cheap token test rather than anything clever: the extraction
/// prompt teaches the wording ("The user's mother Florence lives in Kisumu"),
/// so facts written the way the prompt asks for them pass. Callers should use
/// this to DEMOTE rather than reject — a demotion is reversible by
/// consolidation, a rejection loses the fact forever.
pub fn names_user(content: &str) -> bool {
    names_subject(content, &[])
}

/// Whether a fact names the person the window was about.
///
/// The widening [`names_user`] needed once the prompt stopped saying "the
/// user". The batch prompt tells the model to write the subject BY NAME —
/// "Jerry's sister is Amara" — and the literal-token test would have read every
/// one of those as a fact about somebody else, demoting `relationship`,
/// `preference` and `context` to `knowledge`: the exact bin the catalogue says
/// the extractor may never produce.
///
/// `aliases` are the names the window's subject goes by. They are matched
/// token-wise and case-insensitively, with a possessive tolerated, because a
/// substring match on a short name finds it inside other words ("Al" inside
/// "also"). The `user` tokens stay accepted whatever the aliases are: an
/// unnamed pond still writes "the user", and the 379 rows already in the store
/// were all written that way.
pub fn names_subject(content: &str, aliases: &[String]) -> bool {
    let tokens = split_tokens(content);
    tokens.iter().any(|(_, normalised)| {
        // "user's" survives split_tokens as one token (internal apostrophes
        // are kept on purpose), and the prompt's own canonical example is
        // "The user's mother Florence lives in Kisumu." — so the possessive
        // has to match or the example the model is taught would fail.
        if matches!(normalised.as_str(), "user" | "users" | "user's" | "users'") {
            return true;
        }
        let stem = normalised
            .strip_suffix("'s")
            .or_else(|| normalised.strip_suffix("s'"))
            .unwrap_or(normalised);
        aliases.iter().any(|alias| {
            // A multi-word alias ("Aunt Florence") is matched on its first
            // word: `split_tokens` gives one token at a time, and a display
            // name is identified by the part a sentence actually repeats.
            alias
                .split_whitespace()
                .next()
                .is_some_and(|first| first.eq_ignore_ascii_case(stem))
        })
    })
}

// ── Does a reminder cover this note ─────────────────────────────────────────

/// Words that are common enough to appear in two unrelated sentences about one
/// household, and so cannot be evidence that two of them are about one thing.
///
/// Short and deliberately conservative in the opposite direction from the date
/// lists: an entry here can only ever cause a date to be counted LOST, and an
/// over-count of loss is visible on the Memory panel and correctable. A missing
/// entry is the bug this list exists to avoid -- a note reported as kept
/// because it shared the word "plans" with a reminder about something else.
///
/// Nothing under four characters is here, because nothing under four characters
/// is consulted: the length rule in [`is_distinctive_token`] drops those first.
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
///
/// Three exclusions, each for its own reason:
///  - anything a calendar name -- a weekday, a month, an ordinal, a duration
///    unit, a relative day, a bare number. Every dated note and every reminder
///    carries one, so sharing one says nothing: two unrelated appointments in
///    one window are both "next Tuesday".
///  - the subject's own aliases. The write gate REQUIRES a subject bin's note to
///    name the subject, so "Jerry" is in almost every note this is asked about,
///    and reminders name them too.
///  - words too common to mean anything, and words too short to be safe.
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

/// The distinctive words of a sentence, singularised so "tractors" and
/// "tractor" are one word.
fn distinctive_tokens(content: &str, aliases: &[String]) -> Vec<String> {
    split_tokens(content)
        .into_iter()
        .filter(|(_, lower)| is_distinctive_token(lower, aliases))
        .map(|(_, lower)| singular(&lower).to_string())
        .collect()
}

/// Whether a stored reminder is plausibly ABOUT this refused note.
///
/// # Why this exists
///
/// A dated note is refused whole and the date survives only as a reminder, so
/// "was this date kept" is a question about ONE note, not about the window it
/// arrived in. The window test it replaces read "some reminder landed" and
/// applied that to every dated note in the window: two dated notes and one
/// reminder reported both as kept, and the second date left the pond with every
/// counter reading clean.
///
/// # What the rule is
///
/// One shared distinctive word. The reminder's `about` text and the note are
/// each reduced to their content words -- calendar names, the subject's own
/// names, common words and anything under four characters removed -- and the
/// note is covered when any survives in both.
///
/// # What it can and cannot do
///
/// It CAN tell apart two dated notes in one window that are about different
/// things, which is the whole failure it was written for: "the dentist" does
/// not cover "collecting the tractor on 3 March".
///
/// It CANNOT do any of these, and none of them is a bug to be fixed here:
///  - **Synonyms and paraphrase.** A reminder about "the surgery" does not
///    cover a note about "the dentist". This under-counts: the date IS in the
///    store and the pond reports it lost. That is the deliberate direction --
///    an over-count of loss is a visible banner, an under-count is the silent
///    discard being fixed.
///  - **Two events sharing a noun.** "service the tractor" covers "collecting
///    the tractor on 3 March", because one shared noun is all this can see.
///    That is the one direction it over-reports, and it needs a window
///    containing two different dated events about the same object.
///  - **Morphology beyond a plural `s`.** "collecting" and "collect" are two
///    words to it.
///
/// Matching by wording is exactly the judgement the date word lists kept
/// getting wrong, which is why this is a coarse overlap test with the
/// uncertainty resolved towards LOST, and never an attempt to read the
/// sentence.
pub fn reminder_covers_note(about: &str, note: &str, subject_aliases: &[String]) -> bool {
    let note_words = distinctive_tokens(note, subject_aliases);
    if note_words.is_empty() {
        // Nothing to match on. A note made entirely of a name and a date is a
        // note this cannot connect to anything, and the honest answer is that
        // the date was not shown to be kept.
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

/// How many proper nouns sit *before* `idx` and could be the antecedent.
///
/// A capitalised word is the only antecedent signal available without a parser.
/// Position 0 does not count (every sentence starts capitalised) and "I" names
/// nothing. Counting only what precedes matters: an anaphor cannot be resolved
/// by a name that comes after it, and the old "any capital anywhere" test
/// forgave the dangling phrase whenever the sentence happened to mention a
/// person, city, month or weekday — which is most real facts.
///
/// Position is the *only* thing counted. Asking additionally what kind of
/// antecedent it is — a proper noun in a locative phrase, for "there" and place
/// deictics — destroyed facts whose place is introduced by a copula rather than
/// a preposition ("The user's home town is Kisumu and his parents still live
/// there."). Resolving a deictic to the wrong earlier name costs one vague row;
/// the kind test cost whole correct facts.
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
                // "the former Yugoslavia" names its referent. Otherwise the
                // word selects between two earlier candidates, so it needs two:
                // "moved from Nairobi to Kisumu and prefers the latter" reads
                // on its own, "mother Florence lives in the latter city" does
                // not, however many other capitals the sentence carries.
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
            // Any earlier proper noun resolves it. See [`antecedents_before`]
            // for why the kind of noun is deliberately not inspected.
            if DEICTIC_BIGRAMS.contains(&(lower.as_str(), following.as_str()))
                && antecedents_before(tokens, idx) == 0
            {
                return true;
            }
        }
    }
    false
}

/// Cosine similarity between two embedding vectors.
///
/// Returns `0.0` for mismatched dimensions, empty inputs, or a zero vector —
/// callers treat "no usable embedding" and "unrelated" identically, and the
/// mock embedding provider legitimately returns all-zero vectors.
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

/// The kind of causal or structural relationship between two memories.
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

/// A directed edge between two [`MemoryFragment`]s in the causal graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEdge {
    /// Source memory ID (the *from* end of the directed edge).
    pub from_id: String,
    /// Target memory ID (the *to* end of the directed edge).
    pub to_id: String,
    /// Semantic type of the relationship.
    pub relation: EdgeRelation,
    /// ISO-8601 timestamp when this edge was created.
    pub created_at: String,
}

/// A subgraph of the memory DAG — a set of nodes and the edges that connect them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryGraph {
    /// The memory fragments in this subgraph.
    pub nodes: Vec<MemoryFragment>,
    /// The edges connecting nodes in this subgraph.
    pub edges: Vec<MemoryEdge>,
}

// ── Memory audit log ────────────────────────────────────────────────────────

/// Tracks memory lifecycle events for audit and debugging.
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

/// A single audit log entry for a memory lifecycle event.
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

    /// Whatever the prompt demonstrates, the gate refuses verbatim.
    ///
    /// The gate is now vacuous BY CONSTRUCTION, and the construction is the
    /// point: the batch prompt shows a schema skeleton with `...` for every
    /// value and no worked example at all, so there is no example output for a
    /// copying model to echo. `EXTRACTION_EXAMPLE_FACTS` is therefore empty,
    /// and this test says what the gate would still do if it were not — which
    /// is what keeps the machinery honest rather than merely unused.
    ///
    /// The coupling in the other direction — a prompt that grows an example
    /// must add its output to the list — is asserted in `pond-server`'s
    /// `conversation_extractor`, the one place that can see both.
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

    /// The guard must be an EXACT match, not a resemblance.
    ///
    /// Someone really can have a mother called Florence, or move to Kisumu.
    /// Refusing anything that merely looks like an example would quietly lose
    /// true memories — a worse failure than the one being prevented, because it
    /// is invisible to the user and to us.
    ///
    /// The Kisumu sentences are here for a second reason now: they are what the
    /// old prompt demonstrated, and with that prompt gone they are ordinary
    /// facts again. A gate that kept refusing them would be refusing a real
    /// household's real mother.
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

    /// The subject test that the write gate never had. Every one of these
    /// strings was in the device store, filed in a segment that means "about
    /// the user".
    #[test]
    fn names_user_separates_facts_about_the_user_from_everything_else() {
        // Real user facts — the wording the extraction prompt teaches.
        assert!(names_user("The user's location is Nairobi."));
        assert!(names_user("User prefers concise greetings"));
        assert!(names_user("The user's mother Florence lives in Kisumu."));
        assert!(names_user("The users' shared calendar is on Google."));

        // What was landing in `identity` and `relationship` instead.
        assert!(!names_user("William Ruto is a Kenyan politician."));
        assert!(!names_user("William Ruto is the leader of Kenya."));
        assert!(!names_user("AI assistant"));
        assert!(!names_user("I am a computer program designed to assist"));
        assert!(!names_user("Kirk Lazarus is an Armenian Australian artist"));
    }

    /// "user's" must match: split_tokens keeps internal apostrophes on purpose,
    /// and the prompt's own canonical example is possessive.
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
        // Dimension mismatch, empty input, and the mock provider's zero vector
        // must all be "unrelated", never NaN — NaN would poison every sort.
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
        // Old fragments without new fields should deserialize fine.
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
        let mut frag = MemoryFragment::from_extraction(
            "c2".to_string(),
            None,
            "Likes tea not coffee".to_string(),
            MemorySegment::Preference,
            0.7,
            Some("Likes coffee".to_string()),
        );
        // Segment is Preference but corrects is set
        assert_eq!(frag.segment, Some(MemorySegment::Preference));
        assert!(frag.is_correction());
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

    /// Content a real conversation produces that must survive the gate. The
    /// expensive failure mode here is over-eagerness: a rejected fact is gone.
    const KEEPERS: &[&str] = &[
        "The user's mother lives in Kisumu.",
        "The user's mother's name is Florence.",
        // "therapist" contains "there"; token matching must not see it.
        "The user's therapist is Dr. Amina.",
        "The user thereafter switched to decaf coffee.",
        // "mine" as a noun, "us" as a country, "I" as a numeral. "mine" is not
        // a first-person marker at all any more, so every reading survives.
        "The user works in a mine near Kakamega.",
        "The user works in a coal mine near Kakamega.",
        "The user explored an abandoned mine on a school trip.",
        "The user's uncle owns a gold mine.",
        "The user lives in the US and visits Kenya when the rains break.",
        "The user has Type I diabetes.",
        // Expletive "there", not a place.
        "There is a spare key under the doormat.",
        "The user says there are two dogs in the compound.",
        // "the former" naming its referent, and a resolvable "the latter" —
        // two candidates in the sentence, of any kind.
        "The user grew up in the former Yugoslavia.",
        "The user moved from Nairobi to Kisumu and prefers the latter.",
        "The user compared Rust and Go and prefers the latter.",
        // A "there" whose place is named in the same sentence — introduced by a
        // preposition in the first, by a copula in the second and third.
        "The user moved to Kisumu and still works there.",
        "The user's home town is Kisumu and his parents still live there.",
        "The user's employer is Jarida and the user works there full time.",
        // Words that merely contain a flagged token.
        "The user prefers the shorter route to work.",
        "The user is a formerly published poet.",
        "The user's houseplants are watered every evening.",
    ];

    /// Content pulled from (or modelled on) the junk rows the extractor wrote
    /// into a real memory store.
    const REJECTS: &[(&str, FactDefect)] = &[
        (
            "The user's mother lives in the latter city.",
            FactDefect::UnresolvedReference,
        ),
        // One capitalised token elsewhere in the sentence used to forgive the
        // dangling phrase; it names a person, not the city.
        (
            "The user's mother Florence lives in the latter city.",
            FactDefect::UnresolvedReference,
        ),
        // First-person contractions: one token each, so the bare-pronoun
        // matcher never saw them.
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
        // The last-resort date rung. These reach `fact_defect` only when the
        // caller forgot to strip first, which is the case it exists for.
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
        // Same trailing clause; only the presence of an antecedent differs.
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
        // All four are durable undertakings that happen to be phrased as
        // imperatives. Demoting them files them as Context, Short tier, and
        // they decay out of the store inside a week.
        for durable in [
            "Build a treehouse for the children this summer",
            "Write a novel about beekeeping",
            "Run the Nairobi marathon in October",
            "Design the new logo for Jarida",
            "Learn Swahili before the trip to Mombasa",
            // Verbs that sound like assistant work and are not: keying on the
            // verb alone demoted both of these to Context, Short tier.
            "Convert the garage into a workshop this year",
            "Install the solar panels on the roof before the rains",
        ] {
            assert!(!is_captured_request(durable), "demoted: {durable:?}");
        }
    }

    #[test]
    fn assistant_work_is_still_demoted() {
        // An imperative opener plus a named artifact in the object. The artifact
        // is the only signal now: the verb on its own could not tell "Install
        // the solar panels" from "Install the dependencies".
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
        // The cost of requiring the artifact noun: these three are captured
        // requests and are no longer demoted, so they sit in Project until
        // consolidation retires them. A stale Project row is the cheaper error —
        // the alternative demoted real year-long undertakings.
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
        // Deliberate, and the whole point of dropping the disambiguation: every
        // rule that tried to separate the noun from the pronoun leaked in one
        // direction or the other (a premodifier stack hid "a coal mine"; a
        // backward walk past "of" let "a friend of mine" through). Keeping the
        // noun reading is worth storing the handful of pronoun sentences,
        // because the pronoun case costs one imprecise row and the noun case
        // destroyed a correct fact outright.
        for kept in [
            "The user works in a coal mine near Kakamega.",
            "The user explored a very old abandoned mine.",
            // Genuinely first person, and now stored anyway.
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
        // Positional counting only. "Peter" is a person, not a place, so this
        // row is vaguer than we would like — but demanding a *place* antecedent
        // (a proper noun after a locative preposition) threw away every fact
        // whose place arrives through a copula, which is most of them.
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

    /// The dated shapes the local models were MEASURED to write into a note.
    ///
    /// Six models, 432 live replies through this prompt and this parser. Every
    /// row here is a class that came back with a date in the `note` field, and
    /// every one of them is refused whole -- the sentence is never edited, so
    /// there is nothing to check about what survived.
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

    /// A recurrence is a habit, and it reaches the store as the model wrote it.
    ///
    /// This is the half the stripper destroyed four times over: "swims each
    /// Saturday morning" became "The user swims morning." A weekday inside a
    /// pattern names no day on any calendar -- there is nothing to diary and
    /// nothing in it can become false -- and the models keep writing it that
    /// way: 12 recurring windows of 12 across the shipped model and the larger
    /// one, at greedy and at both seeds. So the prompt asks for the timing to
    /// be kept, and the gate keeps it.
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
            // The clock time belongs to the habit too, and the shipped model
            // writes this shape on 3 windows of 3.
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

    /// The numbers and names a household writes that are NOT dates.
    ///
    /// Every row here was being MANGLED into the store by one of the four
    /// stripper passes, and each one is a false positive the detector must not
    /// have: a false positive now costs the whole fact.
    #[test]
    fn the_not_dates_the_stripper_kept_eating() {
        for note in [
            // Pass 3: any number after "at" at a clause end was a clock hour,
            // and nothing capped it at 24. The six models return these clean
            // on 36 windows of 36.
            "The user keeps the oven at 180.",
            "The user sets the thermostat at 21.",
            "The user keeps the tyres at 40 psi.",
            // A relative-time word list ate the cat.
            "The user's cat is called Midnight.",
            // The modal, which a word list reads as the month of May.
            "The user may travel to Kisumu.",
            // Birth order, which the ordinal rule took as a day of the month.
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

    /// The boundary this change draws on purpose, written down where it will be
    /// found.
    ///
    /// A year used as a name is refused along with a year used as a date. From
    /// inside a token stream "the 2019 model of the tractor" and "moved to
    /// Kisumu in 2019" are the same four digits, and the pass that tried to
    /// separate them stored "The user prefers model of the tractor."
    ///
    /// The same goes for a digit ordinal: "on the 4th floor" and "on the 4th"
    /// are one shape.
    ///
    /// Both are refusals of a true fact, and both are cheaper than the
    /// alternative: the fact is still in the conversation and can be extracted
    /// again, while a stored year is read back for as long as the pond runs.
    /// The model is the only thing that could tell these apart, which is what
    /// the prompt now asks it to do -- it is measured putting the specific date
    /// in `reminders` on 31 of its 36 dated windows, and losing no date at all.
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

    /// Nothing about the detector edits anything.
    ///
    /// The property the whole change is for, asserted as a property rather than
    /// left as a description: the gate's only output is yes or no, and what the
    /// caller stores is the model's own sentence.
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
        // The measured case: one window, two dated notes, one reminder. The
        // tractor date is not in the store and must not be reported as kept.
        assert!(!reminder_covers_note(
            "the dentist",
            "Jerry is collecting the tractor on 3 March.",
            &jerry()
        ));
    }

    #[test]
    fn a_shared_date_alone_is_not_a_match() {
        // Two different Tuesday appointments share every word this could match
        // on except the one that matters.
        assert!(!reminder_covers_note(
            "the dentist on Tuesday",
            "Jerry sees the farrier next Tuesday.",
            &jerry()
        ));
    }

    #[test]
    fn the_subjects_own_name_is_not_a_match() {
        // Every note in a subject bin names the subject -- the write gate
        // requires it -- so a name can never be the evidence.
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
        // Documented limit, asserted so it stays a known shape rather than a
        // surprise: this under-reports keeping, never over-reports it.
        assert!(!reminder_covers_note(
            "the surgery",
            "Jerry has a dentist appointment next Tuesday.",
            &jerry()
        ));
    }
}
