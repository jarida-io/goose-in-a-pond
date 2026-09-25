//! Memory fragments, their classification and decay tiers, and the fact quality gate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

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
            Self::Project => 0.6,
            Self::Knowledge => 0.5,
            Self::Context => 0.3,
        }
    }

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
        }
    }

    /// True for user corrections, which consolidation must never prune or merge away.
    pub fn is_correction(&self) -> bool {
        self.segment.as_ref() == Some(&MemorySegment::Correction) || self.corrects.is_some()
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
        }
    }
}

// ── Fact quality gate ───────────────────────────────────────────────────────

/// Shortest trimmed content, in characters, that can carry a fact.
pub const MIN_FACT_CONTENT_LEN: usize = 8;

/// Why a candidate memory was refused at write time: out of context it would mislead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactDefect {
    /// Nothing left after normalisation, or too short to carry a fact.
    TooShort,
    /// A deictic or leading pronoun with no antecedent in the sentence ("the latter city").
    UnresolvedReference,
    /// First person ("my mother"): injected into context, "my" reads as the assistant's.
    FirstPerson,
    /// A verbatim copy of the extraction prompt's worked example. Small models copy examples,
    /// and these facts pass every other check, so they are refused outright.
    EchoedExample,
}

impl FactDefect {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TooShort => "too short",
            Self::UnresolvedReference => "unresolved reference",
            Self::FirstPerson => "first person",
            Self::EchoedExample => "echoed the prompt's own example",
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
    None
}

/// Facts from the extraction prompt's worked example. Only exact, case-insensitive echoes are
/// refused: a real user may well have a mother called Florence.
const EXTRACTION_EXAMPLE_FACTS: &[&str] = &[
    "The user's mother Florence lives in Kisumu.",
    "The user moved to Kisumu in 2019.",
];

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
    split_tokens(content).iter().any(|(_, normalised)| {
        // split_tokens keeps "user's" whole, and the prompt teaches the possessive form.
        matches!(normalised.as_str(), "user" | "users" | "user's" | "users'")
    })
}

/// True for a copied one-off request: a task-verb opener AND an assistant-artifact object.
/// Errs toward false, as a demoted real project decays away within a week.
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
    fn the_extraction_examples_own_facts_are_refused() {
        for content in [
            "The user's mother Florence lives in Kisumu.",
            "The user moved to Kisumu in 2019.",
            // Case and padding must not get a copy through.
            "  the user's mother florence lives in kisumu.  ",
        ] {
            assert_eq!(
                fact_defect(content),
                Some(FactDefect::EchoedExample),
                "{content:?} is the prompt's own demonstration, not a fact about \
                 this user"
            );
        }
    }

    #[test]
    fn a_real_fact_that_resembles_the_example_still_passes() {
        for content in [
            "The user's mother Florence lives in Nakuru.",
            "The user's sister Florence lives in Kisumu.",
            "The user moved to Kisumu in 2021.",
            "The user's mother is called Florence.",
        ] {
            assert_eq!(
                fact_defect(content),
                None,
                "{content:?} is a real fact that merely resembles the example"
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
        let mut frag = MemoryFragment::from_extraction(
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
        "The user explored an abandoned mine last year.",
        "The user's uncle owns a gold mine.",
        "The user lives in the US and visits Kenya each August.",
        "The user has Type I diabetes.",
        // Expletive "there", not a place.
        "There is a spare key under the doormat.",
        "The user says there are two dogs in the compound.",
        // "the former" naming its referent; "the latter" with two candidates of any kind.
        "The user grew up in the former Yugoslavia.",
        "The user moved from Nairobi to Kisumu and prefers the latter.",
        "The user compared Rust and Go and prefers the latter.",
        // "there" with its place named earlier, via a preposition or a copula.
        "The user moved to Kisumu in 2019 and still works there.",
        "The user's home town is Kisumu and his parents still live there.",
        "The user's employer is Jarida and the user works there full time.",
        // Words that merely contain a flagged token.
        "The user prefers the shorter route to work.",
        "The user is a formerly published poet.",
        "The user's houseplants are watered every evening at 6 PM.",
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
        assert!(fact_defect("The user's brother Peter enjoyed that place in March.").is_none());
        assert!(
            fact_defect("The user's home town is Kisumu and his parents still live there.")
                .is_none()
        );
        // With nothing named before it, the deictic is still dangling.
        assert!(fact_defect("The user's brother enjoyed that place in March.").is_some());
        assert!(fact_defect("The user's brother lives there.").is_some());
    }
}
