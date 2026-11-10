//! Locked baseline for the memory-extraction write gate.
//!
//! Replays a synthetic household history through the real gate and checks every metric
//! against `fixtures/memory-reachability/baseline.json`. Real: the batch engine (via
//! `run_pass`), `select_for_turn`, `CompactionProfile`. Stand-ins: the model
//! (`replies.jsonl`), the embedder ([`HashEmbedder`]) and a copy of
//! `GooseAgent::topical_memories` (the adapter needs Goose). The parser is not measured: the
//! fixtures keep the old schema and [`as_window_extraction`] translates them. Never edit
//! t01-t29, only append, and date new turns across the history so recency stays comparable.
//!
//! Print the baseline with:
//!     cargo test -p pond-infra --test memory_reachability_corpus -- --nocapture

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::models::domain::embedding_models::{SimilarityFloors, ALL_MINILM_L6_V2};
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::models::services::context::context_budget::CompactionProfile;
use pond_core::user_data::domain::memory::{
    cosine_similarity, fact_defect, normalise_fact_content, FactDefect, MemoryFragment,
    MemorySegment,
};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::session::SessionMessage;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_session::InMemorySessionStorage;
use pond_core::user_data::ports::conversation_extractor::{
    ConversationExtractor, ExtractedMemory, ExtractionError, ExtractionWindow, MemoryKind,
    WindowExtraction, WindowSubject,
};
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::services::memory_extraction::{
    BatchExtractionConfig, BatchExtractionService,
};
use pond_core::user_data::services::memory_relevance::{
    content_tokens, is_duplicate_content, select_for_turn,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

const MANIFEST: &str = include_str!("fixtures/memory-reachability/manifest.json");
const CONVERSATIONS: &str = include_str!("fixtures/memory-reachability/conversations.jsonl");
const REPLIES: &str = include_str!("fixtures/memory-reachability/replies.jsonl");
const QUERIES: &str = include_str!("fixtures/memory-reachability/queries.jsonl");
const EXPECTATIONS: &str = include_str!("fixtures/memory-reachability/expectations.jsonl");
const BASELINE: &str = include_str!("fixtures/memory-reachability/baseline.json");

/// `memory_extraction_max_facts`'s default, pinned so the baseline doesn't track `Settings`.
const MAX_FACTS_PER_TURN: usize = 3;

/// `agent_memory_limit`'s default: fragments reaching the prompt (the 5 in recall@5).
const INJECTION_LIMIT: usize = 5;

/// `MEMORY_CANDIDATE_FANOUT` and `MEMORY_CANDIDATE_FLOOR` from `goose_agent.rs`.
const CANDIDATE_FANOUT: usize = 8;
const CANDIDATE_FLOOR: usize = 40;

/// Same/Related band thresholds as designed, pinned here rather than read from the engine.
const SAME_BAND: f32 = 0.94;
const RELATED_BAND: f32 = 0.78;

// ── Fixtures ────────────────────────────────────────────────────────────────

/// The corpus's anchored "now"; the wall clock would make recency scores drift daily.
fn corpus_now() -> DateTime<Utc> {
    let m: Value = serde_json::from_str(MANIFEST).expect("manifest");
    m["corpus_now"]
        .as_str()
        .expect("corpus_now")
        .parse()
        .expect("corpus_now parses")
}

struct Turn {
    turn_id: String,
    session_id: String,
    at: DateTime<Utc>,
    user: String,
    assistant: String,
}

fn turns() -> Vec<Turn> {
    CONVERSATIONS
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("conversation line");
            Turn {
                turn_id: v["turn_id"].as_str().unwrap().to_string(),
                session_id: v["session_id"].as_str().unwrap().to_string(),
                at: v["at"].as_str().unwrap().parse().expect("timestamp"),
                user: v["user"].as_str().unwrap().to_string(),
                assistant: v["assistant"].as_str().unwrap().to_string(),
            }
        })
        .collect()
}

/// Recorded model output, keyed by turn.
fn replies() -> BTreeMap<String, String> {
    REPLIES
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("reply line");
            (
                v["turn_id"].as_str().unwrap().to_string(),
                v["raw"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

struct Query {
    query: String,
    relevant: Vec<String>,
}

fn queries() -> Vec<Query> {
    QUERIES
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("query line");
            Query {
                query: v["query"].as_str().unwrap().to_string(),
                relevant: v["relevant"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r.as_str().unwrap().to_string())
                    .collect(),
            }
        })
        .collect()
}

struct Expectation {
    fact_key: String,
    expect: String,
    to_segment: Option<String>,
    defect: Option<String>,
    note: String,
}

fn expectations() -> Vec<Expectation> {
    EXPECTATIONS
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("expectation line");
            Expectation {
                fact_key: v["fact_key"].as_str().unwrap().to_string(),
                expect: v["expect"].as_str().unwrap().to_string(),
                to_segment: v.get("to_segment").and_then(|s| s.as_str()).map(Into::into),
                defect: v.get("defect").and_then(|s| s.as_str()).map(Into::into),
                note: v
                    .get("note")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default()
                    .to_string(),
            }
        })
        .collect()
}

// ── The stand-in embedder ───────────────────────────────────────────────────

/// Deterministic bag-of-tokens embedding over `content_tokens`, minus "user": every fact
/// names the user, so it would lift every pair's cosine by the same meaningless amount.
struct HashEmbedder {
    dims: usize,
}

impl HashEmbedder {
    fn new() -> Self {
        Self { dims: 256 }
    }
}

#[async_trait]
impl EmbeddingProvider for HashEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = vec![0.0f32; self.dims];
        for token in content_tokens(text) {
            if token == "user" {
                continue;
            }
            // FNV-1a by hand: `DefaultHasher` isn't stable across Rust releases.
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in token.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            v[(h % self.dims as u64) as usize] += 1.0;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        Ok(v)
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn model_id(&self) -> String {
        "harness-hash-256".to_string()
    }

    /// Stands in for the shipped MiniLM, so the gate runs its cosine bands as a pond does.
    fn similarity_floors(&self) -> Option<SimilarityFloors> {
        ALL_MINILM_L6_V2.floors
    }
}

// ── The recorded extractor ──────────────────────────────────────────────────

/// Answers each one-turn window (see [`build_corpus`]) with that turn's recorded facts.
struct RecordedExtractor {
    by_window: BTreeMap<String, WindowExtraction>,
}

#[async_trait]
impl ConversationExtractor for RecordedExtractor {
    async fn extract_window(
        &self,
        window: ExtractionWindow<'_>,
    ) -> std::result::Result<WindowExtraction, ExtractionError> {
        Ok(self
            .by_window
            .get(window.window_id)
            .cloned()
            .expect("the corpus has no recorded reply for this window"))
    }
}

/// Translate an old-schema recorded reply into what the batch engine reads (its parser
/// rejects that schema). `identity`/`knowledge` map to `context` so the subject gate sees them.
fn as_window_extraction(raw: &str) -> WindowExtraction {
    let mut extraction = WindowExtraction::default();
    for value in recorded_facts(raw) {
        let Some(content) = value
            .get("content")
            .or_else(|| value.get("fact"))
            .and_then(|f| f.as_str())
        else {
            continue;
        };
        let Some(kind) = value
            .get("segment")
            .and_then(|s| s.as_str())
            .and_then(as_kind)
        else {
            // The parser counts an unknown label as rejected; match it.
            extraction.rejected += 1;
            continue;
        };
        extraction.memories.push(ExtractedMemory {
            note: content.to_string(),
            kind,
        });
    }
    extraction
}

/// Parse an expected segment: any of the seven stored ones, as demotion can land in `knowledge`.
fn parse_segment(s: &str) -> Option<MemorySegment> {
    match s.to_lowercase().as_str() {
        "identity" => Some(MemorySegment::Identity),
        "preference" => Some(MemorySegment::Preference),
        "correction" => Some(MemorySegment::Correction),
        "relationship" => Some(MemorySegment::Relationship),
        "routine" => Some(MemorySegment::Routine),
        "project" => Some(MemorySegment::Project),
        "knowledge" => Some(MemorySegment::Knowledge),
        "context" => Some(MemorySegment::Context),
        _ => None,
    }
}

fn as_kind(segment: &str) -> Option<MemoryKind> {
    match segment.to_lowercase().as_str() {
        "relationship" => Some(MemoryKind::Relationship),
        "preference" => Some(MemoryKind::Preference),
        "correction" => Some(MemoryKind::Correction),
        "project" => Some(MemoryKind::Routine),
        "identity" | "knowledge" | "context" => Some(MemoryKind::Context),
        _ => None,
    }
}

/// Pull the facts array out of a recorded reply; fixture parsing, not the production parser.
fn recorded_facts(raw: &str) -> Vec<Value> {
    let cleaned = match (raw.find("<think>"), raw.find("</think>")) {
        (Some(open), Some(close)) => format!("{}{}", &raw[..open], &raw[close + 8..]),
        _ => raw.to_string(),
    };
    let cleaned = cleaned.trim();
    facts_array(cleaned)
        .or_else(|| {
            let start = cleaned.find('{')?;
            let end = cleaned.rfind('}')?;
            facts_array(&cleaned[start..=end])
        })
        .unwrap_or_default()
}

fn facts_array(text: &str) -> Option<Vec<Value>> {
    let v: Value = serde_json::from_str(text).ok()?;
    if let Some(arr) = v.get("facts").and_then(|f| f.as_array()) {
        return Some(arr.clone());
    }
    v.as_array().cloned()
}

/// What the write gate stores for a recorded fact: whitespace collapsed, label prefix dropped.
fn as_stored(content: &str) -> String {
    normalise_fact_content(content)
}

/// The write gate's verdict on a recorded fact; mirrors `memory_extraction`'s gate.
fn write_gate_verdict(content: &str) -> Option<FactDefect> {
    fact_defect(&normalise_fact_content(content))
}

// ── The date auditor ────────────────────────────────────────────────────────

const MONTHS: &[&str] = &[
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
    "sep",
    "sept",
    "oct",
    "nov",
    "dec",
];

const WEEKDAYS: &[&str] = &[
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

const RELATIVE: &[&str] = &["tomorrow", "yesterday", "tonight", "today"];

/// Spelled days and hours are dates only after a lead word ("on the fourteenth", "at six").
const SPELLED_DAYS: &[&str] = &[
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
    "thirtieth",
];

const SPELLED_HOURS: &[&str] = &[
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
    "twelve",
];

const TIME_UNITS: &[&str] = &[
    "day",
    "days",
    "week",
    "weeks",
    "month",
    "months",
    "year",
    "years",
    "fortnight",
];

/// A day of the month written plainly: 1 to 31.
fn is_day_number(w: &str) -> bool {
    w.len() <= 2
        && w.chars().all(|c| c.is_ascii_digit())
        && (1..=31).contains(&w.parse().unwrap_or(0))
}

/// Parts of a separated number for the year test; none for a dotted version like "2019.1".
fn numeric_parts(w: &str) -> Vec<&str> {
    for sep in ['-', '/', '.'] {
        if !w.contains(sep) {
            continue;
        }
        let parts: Vec<&str> = w.split(sep).collect();
        if sep == '.' && parts.len() == 2 {
            return Vec::new();
        }
        return parts;
    }
    vec![w]
}

/// Token positions of weekdays/months that recur ("each Saturday"): a habit, not a date.
/// A missed form reports a false leak that the gate could only fix by destroying the habit.
fn recurrence_positions(words: &[&str]) -> BTreeSet<usize> {
    let named = |w: &str| MONTHS.contains(&w) || WEEKDAYS.contains(&w);
    let mut out = BTreeSet::new();
    for (i, w) in words.iter().enumerate() {
        // Plural: "on Saturdays", "on weekdays"; a plural cannot name one day.
        if w.strip_suffix('s').is_some_and(named) || *w == "weekdays" || *w == "weekends" {
            out.insert(i);
            continue;
        }
        if !named(w) {
            continue;
        }
        let before = i.checked_sub(1).map(|j| words[j]);
        // Marked: "each Saturday", "every March".
        if matches!(before, Some("each" | "every")) {
            out.insert(i);
            continue;
        }
        // A span: "from Monday to Friday".
        if words.get(i + 1) == Some(&"to") && words.get(i + 2).is_some_and(|n| named(n)) {
            out.insert(i);
            out.insert(i + 2);
            continue;
        }
        // A list continuing one: "each March and October".
        if matches!(before, Some("and" | "or"))
            && i.checked_sub(2).is_some_and(|j| out.contains(&j))
        {
            out.insert(i);
        }
    }
    out
}

/// Whether stored content carries a date; independent of `carries_calendar_date` on purpose.
fn carries_a_date(content: &str) -> bool {
    let lower = content.to_lowercase();
    let words: Vec<&str> = lower
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect();
    let recurring = recurrence_positions(&words);
    // In a habitual sentence ("every Monday at 09:00", "daily at 9am") a clock time belongs to
    // the habit. Only the clock-time rules below consult this; its words must cover the gate's.
    let habitual = !recurring.is_empty()
        || words.iter().any(|w| {
            matches!(
                *w,
                "each" | "every" | "daily" | "nightly" | "weekly" | "monthly"
            )
        });

    for (i, w) in words.iter().enumerate() {
        // A recurring weekday or month is a habit, not a date.
        if recurring.contains(&i) {
            continue;
        }
        // "may" is usually the modal: a month only after a preposition or beside a day number.
        let ambiguous_month = *w == "may"
            && !(i > 0 && matches!(words[i - 1], "in" | "on" | "by" | "since" | "until" | "of"))
            && !words.get(i + 1).is_some_and(|n| is_day_number(n))
            && !(i > 0 && is_day_number(words[i - 1]));
        if (MONTHS.contains(w) && !ambiguous_month) || WEEKDAYS.contains(w) || RELATIVE.contains(w)
        {
            return true;
        }
        // A 19xx/20xx year or decade ("the 1990s"); via `numeric_parts`, also every separated date.
        for part in numeric_parts(w) {
            for candidate in [part, part.strip_suffix('s').unwrap_or(part)] {
                if candidate.len() == 4 && candidate.chars().all(|c| c.is_ascii_digit()) {
                    let n: u32 = candidate.parse().unwrap_or(0);
                    if (1900..=2099).contains(&n) {
                        return true;
                    }
                }
            }
        }
        // "on the fourteenth": a date only if a month or nothing follows ("the second of four").
        if SPELLED_DAYS.contains(w)
            && i > 0
            && matches!(words[i - 1], "the" | "on" | "of" | "by" | "until")
            && (i + 1 == words.len()
                || (words.get(i + 1) == Some(&"of")
                    && words.get(i + 2).is_some_and(|m| MONTHS.contains(m))))
        {
            return true;
        }
        // A numbered day ("the 1st", "on the 14th") is a date even in "the 1st of every month".
        if let Some(digits) = w
            .strip_suffix("st")
            .or_else(|| w.strip_suffix("nd"))
            .or_else(|| w.strip_suffix("rd"))
            .or_else(|| w.strip_suffix("th"))
        {
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
        // "in three weeks", "within 10 days".
        if (SPELLED_HOURS.contains(w) || w.chars().all(|c| c.is_ascii_digit()))
            && i > 0
            && matches!(words[i - 1], "in" | "within")
            && words.get(i + 1).is_some_and(|n| TIME_UNITS.contains(n))
        {
            return true;
        }
        // "next week", "last month" and the rest of the relative set.
        if matches!(*w, "next" | "last" | "this")
            && words
                .get(i + 1)
                .is_some_and(|n| matches!(*n, "week" | "month" | "year" | "monday" | "tuesday"))
        {
            return true;
        }
        // Only clock-time rules below; the relative dates above count even in a habitual sentence.
        if habitual {
            continue;
        }
        // "9am", "11pm" -- one token, so no neighbour rule below can see it.
        for meridiem in ["am", "pm"] {
            if let Some(hour) = w.strip_suffix(meridiem) {
                if !hour.is_empty() && hour.len() <= 2 && hour.chars().all(|c| c.is_ascii_digit()) {
                    return true;
                }
            }
        }
        // "at six", "six pm", "seven o'clock".
        if SPELLED_HOURS.contains(w)
            && (i > 0 && matches!(words[i - 1], "at" | "by" | "around" | "until")
                || words
                    .get(i + 1)
                    .is_some_and(|n| matches!(*n, "am" | "pm" | "o" | "oclock")))
        {
            return true;
        }
        // A clock time, either "09:00" or a bare hour followed by am/pm.
        if w.contains(':') {
            let parts: Vec<&str> = w.split(':').collect();
            if parts.len() == 2
                && !parts[0].is_empty()
                && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
            {
                return true;
            }
        }
        if (*w == "am" || *w == "pm")
            && i > 0
            && words[i - 1].chars().all(|c| c.is_ascii_digit())
            && !words[i - 1].is_empty()
        {
            return true;
        }
    }
    false
}

// ── Building the store ──────────────────────────────────────────────────────

/// What the write gate did with one recorded fact.
#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    /// Reached the store, under this segment.
    Stored(MemorySegment),
    /// Refused by `fact_defect`, in the parser or at the gate.
    Refused(String),
    /// Parsed and offered, then dropped as a duplicate of something stored.
    Deduped,
}

struct Corpus {
    /// Every stored fragment, `created_at` restamped to its turn's time for the recency term.
    fragments: Vec<MemoryFragment>,
    /// fact key -> what happened to it.
    outcomes: BTreeMap<String, Outcome>,
    /// fact key -> the recorded content, before the gate saw it.
    recorded: BTreeMap<String, String>,
    /// fact key -> stored fragment id, for keys that reached the store.
    stored_ids: BTreeMap<String, String>,
    /// fact key -> its highest cosine against the store at offer time (afterwards, dedup has
    /// removed the near-duplicates the bands exist to catch).
    best_sim_at_offer: BTreeMap<String, f32>,
}

/// Replay the history through the real write gate, one turn per `run_pass`: given all turns,
/// `order_pass` reads newest first and would store restatements before their originals.
async fn build_corpus() -> Corpus {
    let turns = turns();
    let replies = replies();

    // Every recorded fact, keyed and in order.
    let mut recorded: BTreeMap<String, String> = BTreeMap::new();
    for t in &turns {
        let raw = replies
            .get(&t.turn_id)
            .expect("a turn with no recorded reply");
        // `Deduped` is inferred by elimination, so a turn overflowing the cap would be mislabelled.
        let offered = as_window_extraction(raw);
        let well_formed = offered
            .memories
            .iter()
            .filter(|m| write_gate_verdict(&m.note).is_none())
            .count();
        assert!(
            well_formed <= MAX_FACTS_PER_TURN,
            "{} offers {well_formed} well-formed memories against a cap of \
             {MAX_FACTS_PER_TURN}; the overflow would be recorded as a duplicate",
            t.turn_id
        );
        for (i, memory) in offered.memories.iter().enumerate() {
            recorded.insert(format!("{}:{}", t.turn_id, i), memory.note.clone());
        }
    }

    let embedder = HashEmbedder::new();
    let service = BatchExtractionService::new()
        .with_embedding_provider(Arc::new(HashEmbedder::new()) as Arc<dyn EmbeddingProvider>);
    let repo = MockMemoryRepository::new();

    // Shipped defaults; the anonymous subject, since every fact says "The user ...".
    let mut settings = Settings::default();
    settings.memory_extraction_window_messages = 2;
    settings.memory_extraction_sessions_per_pass = 1;
    let config = BatchExtractionConfig::from_settings(&settings);
    assert!(
        config.mode.writes(),
        "the shipped mode no longer writes, so this harness is measuring an empty store"
    );
    assert_eq!(config.fallback_subject, WindowSubject::anonymous());

    let mut best_sim_at_offer: BTreeMap<String, f32> = BTreeMap::new();
    let mut outcomes: BTreeMap<String, Outcome> = BTreeMap::new();
    let mut stored_ids: BTreeMap<String, String> = BTreeMap::new();
    let mut stamped: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();

    for t in &turns {
        let raw = replies.get(&t.turn_id).unwrap();
        let offered = as_window_extraction(raw);
        let window_id = format!("{}-a", t.turn_id);

        // The store as the engine sees it when this window is offered.
        let snapshot = repo
            .search_recent(&ProfileScope::Household, usize::MAX)
            .await
            .expect("mock read");
        for (i, memory) in offered.memories.iter().enumerate() {
            let v = embedder
                .embed(&as_stored(&memory.note))
                .await
                .expect("embed candidate");
            let best = snapshot
                .iter()
                .filter_map(|f| f.embedding.as_deref())
                .map(|e| cosine_similarity(&v, e))
                .fold(0.0f32, f32::max);
            best_sim_at_offer.insert(format!("{}:{}", t.turn_id, i), best);
        }

        let before: BTreeSet<String> = repo
            .search_recent(&ProfileScope::Household, usize::MAX)
            .await
            .expect("mock read")
            .into_iter()
            .map(|f| f.id)
            .collect();

        // A storage holding this turn and nothing else. See the note above.
        let storage = InMemorySessionStorage::new();
        storage
            .create_session(t.session_id.clone())
            .await
            .expect("create session");
        for (suffix, message) in [
            ("u", ChatMessage::user(t.user.clone())),
            ("a", ChatMessage::assistant(t.assistant.clone())),
        ] {
            let mut row = SessionMessage::new(
                format!("{}-{suffix}", t.turn_id),
                t.session_id.clone(),
                message,
            );
            row.created_at = t.at;
            storage
                .add_message(t.session_id.clone(), row)
                .await
                .expect("add message");
        }

        let extractor = RecordedExtractor {
            by_window: [(window_id.clone(), offered.clone())].into_iter().collect(),
        };
        let report = service
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(
            report.windows_examined, 1,
            "{} was not read at all ({report:?})",
            t.turn_id
        );

        // Rows are stored in offer order, so claim them as a subsequence of this turn's facts: a
        // content-keyed map would hand a verbatim restatement's row to the last key.
        let mut fresh: Vec<MemoryFragment> = repo
            .search_recent(&ProfileScope::Household, usize::MAX)
            .await
            .expect("mock read")
            .into_iter()
            .filter(|f| !before.contains(&f.id))
            .collect();
        fresh.sort_by(|a, b| a.created_at.cmp(&b.created_at));

        let mut claimed = vec![false; fresh.len()];
        for (i, memory) in offered.memories.iter().enumerate() {
            let key = format!("{}:{}", t.turn_id, i);
            let want = as_stored(&memory.note);
            let hit = fresh
                .iter()
                .enumerate()
                .position(|(j, f)| !claimed[j] && f.content == want);
            match hit {
                Some(j) => {
                    claimed[j] = true;
                    stored_ids.insert(key.clone(), fresh[j].id.clone());
                    stamped.insert(fresh[j].id.clone(), t.at);
                    outcomes.insert(
                        key,
                        Outcome::Stored(
                            fresh[j].segment.clone().unwrap_or(MemorySegment::Knowledge),
                        ),
                    );
                }
                None => {
                    let outcome = match write_gate_verdict(&memory.note) {
                        Some(d) => Outcome::Refused(d.to_string()),
                        None => Outcome::Deduped,
                    };
                    outcomes.insert(key, outcome);
                }
            }
        }
        assert!(
            claimed.iter().all(|c| *c),
            "{} stored a row matching no recorded fact; the attribution is wrong, not the gate",
            t.turn_id
        );
    }

    let mut fragments = repo
        .search_recent(&ProfileScope::Household, usize::MAX)
        .await
        .expect("mock read");

    // `from_window_extraction` stamps the wall clock, which would collapse the recency term.
    for f in fragments.iter_mut() {
        f.created_at = *stamped
            .get(&f.id)
            .expect("every stored row was claimed above");
    }

    Corpus {
        fragments,
        outcomes,
        recorded,
        stored_ids,
        best_sim_at_offer,
    }
}

// ── Retrieval, as the agent does it ─────────────────────────────────────────

/// `GooseAgent`'s candidate pool for a turn: recency and semantic slices merged by id.
async fn candidates_for(
    query: &str,
    fragments: &[MemoryFragment],
    embedder: &HashEmbedder,
) -> Vec<(MemoryFragment, Option<f32>)> {
    let limit = (INJECTION_LIMIT * CANDIDATE_FANOUT).max(CANDIDATE_FLOOR);
    let qvec = embedder.embed_query(query).await.expect("embed query");

    // Recency slice.
    let mut recent: Vec<MemoryFragment> = fragments.to_vec();
    recent.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    recent.truncate(limit);
    let mut pool: Vec<(MemoryFragment, Option<f32>)> =
        recent.into_iter().map(|f| (f, None)).collect();

    // Semantic slice as `topical_memories` scores it: a width mismatch gets no similarity.
    let mut scored: Vec<(f32, MemoryFragment)> = fragments
        .iter()
        .filter_map(|f| {
            let e = f.embedding.as_ref()?;
            Some((cosine_similarity(&qvec, e), f.clone()))
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    for (_, fragment) in scored {
        let similarity = fragment
            .embedding
            .as_deref()
            .filter(|e| e.len() == qvec.len())
            .map(|e| cosine_similarity(&qvec, e));
        match pool.iter_mut().find(|(f, _)| f.id == fragment.id) {
            Some(entry) => entry.1 = similarity,
            None => pool.push((fragment, similarity)),
        }
    }
    pool
}

/// What reaches the prompt at one context window, costed at chars/4 like the injection loop.
fn within_budget(ranked: &[(MemoryFragment, Option<f32>)], window: usize) -> Vec<&MemoryFragment> {
    let profile = CompactionProfile::from_context_window(window);
    let mut used = 0usize;
    let mut kept = Vec::new();
    for (m, _) in ranked.iter().take(profile.max_memory_fragments) {
        let cost = m.content.len() / 4 + 1;
        if used + cost > profile.memory_token_budget && !kept.is_empty() {
            break;
        }
        used += cost;
        kept.push(m);
    }
    kept
}

// ── The measurements ────────────────────────────────────────────────────────

/// Only planted cases are expected; incidental dedup outcomes are measured by the baseline.
#[tokio::test]
async fn the_write_gate_disposes_of_every_planted_fact_as_the_corpus_expects() {
    let corpus = build_corpus().await;

    let mut failures: Vec<String> = Vec::new();
    println!("\n-- write-gate expectations ----------------------------------");
    for exp in expectations() {
        let outcome = corpus.outcomes.get(&exp.fact_key).unwrap_or_else(|| {
            panic!(
                "expectation names a fact key the corpus does not contain: {}",
                exp.fact_key
            )
        });

        let ok = match (exp.expect.as_str(), outcome) {
            ("rejected", Outcome::Refused(d)) => exp.defect.as_ref().is_none_or(|want| want == d),
            ("stored", Outcome::Stored(_)) => true,
            // Named rather than left to the aggregate, so a dedup regression fails on this row.
            ("deduped", Outcome::Deduped) => true,
            ("demoted", Outcome::Stored(seg)) => exp
                .to_segment
                .as_ref()
                .and_then(|s| parse_segment(s))
                .is_some_and(|want| &want == seg),
            _ => false,
        };
        if !ok {
            failures.push(format!(
                "{}: expected {}{}, got {:?}\n      {}",
                exp.fact_key,
                exp.expect,
                exp.to_segment
                    .as_ref()
                    .map(|s| format!(" to {s}"))
                    .unwrap_or_default(),
                outcome,
                exp.note
            ));
        }
        println!(
            "  {} {:<8} expected {:<9} got {:?}",
            if ok { "ok  " } else { "FAIL" },
            exp.fact_key,
            exp.expect,
            outcome
        );
    }

    assert!(
        failures.is_empty(),
        "the write gate disagreed with the corpus on {} fact(s):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

#[tokio::test]
async fn the_write_gate_baseline() {
    let corpus = build_corpus().await;
    let now = corpus_now();
    let embedder = HashEmbedder::new();
    let fragments = &corpus.fragments;

    // Every printed number, compared against `baseline.json` at the end.
    let mut measured: BTreeMap<String, f64> = BTreeMap::new();

    // ── what the gate let through ────────────────────────────────────────
    let mut stored = 0usize;
    let mut refused: BTreeMap<String, usize> = BTreeMap::new();
    let mut deduped = 0usize;
    for outcome in corpus.outcomes.values() {
        match outcome {
            Outcome::Stored(_) => stored += 1,
            Outcome::Refused(d) => *refused.entry(d.clone()).or_default() += 1,
            Outcome::Deduped => deduped += 1,
        }
    }
    let offered = corpus.outcomes.len();
    println!(
        "\n== BASELINE: the batch write gate, {} turns =================",
        turns().len()
    );
    println!("\n-- what the write gate did ----------------------------------");
    println!(
        "  {offered} facts recorded, {stored} stored, {deduped} deduped, {} refused",
        refused.values().sum::<usize>()
    );
    for (defect, n) in &refused {
        println!("      refused: {defect} x{n}");
    }
    measured.insert("facts_recorded".into(), offered as f64);
    measured.insert("stored".into(), stored as f64);
    measured.insert("deduped".into(), deduped as f64);
    measured.insert("refused".into(), refused.values().sum::<usize>() as f64);
    let by_segment = fragments
        .iter()
        .fold(BTreeMap::new(), |mut m: BTreeMap<String, usize>, f| {
            let s = f
                .segment
                .clone()
                .map(|s| format!("{s:?}").to_lowercase())
                .unwrap_or_else(|| "none".into());
            *m.entry(s).or_default() += 1;
            m
        });
    println!("  stored by segment: {by_segment:?}");

    // ── date_leak_rate ───────────────────────────────────────────────────
    // Must be zero; `recorded_with_dates` must not be, or the zero is vacuous.
    let leaking: Vec<&MemoryFragment> = fragments
        .iter()
        .filter(|f| carries_a_date(&f.content))
        .collect();
    let date_leak_rate = leaking.len() as f32 / fragments.len() as f32;
    println!("\n-- date_leak_rate -------------------------------------------");
    for f in &leaking {
        println!("  LEAK  {}", f.content);
    }
    let recorded_with_dates = corpus
        .recorded
        .values()
        .filter(|c| carries_a_date(c))
        .count();
    println!(
        "  {} of {} stored memories carry a calendar date -- date_leak_rate = {:.3}",
        leaking.len(),
        fragments.len(),
        date_leak_rate
    );
    println!("  ({recorded_with_dates} of the {offered} recorded facts contained one)");
    measured.insert("date_leak_rate".into(), date_leak_rate as f64);

    // ── dup_rate ─────────────────────────────────────────────────────────
    // Stored pairs that `is_duplicate_content` still calls duplicates.
    let mut dup_pairs: Vec<(&str, &str)> = Vec::new();
    for (i, a) in fragments.iter().enumerate() {
        for b in fragments.iter().skip(i + 1) {
            if is_duplicate_content(&a.content, &b.content) {
                dup_pairs.push((a.content.as_str(), b.content.as_str()));
            }
        }
    }
    let dup_rate = dup_pairs.len() as f32 / fragments.len() as f32;

    // The same by cosine, which disagrees with the lexical rule here; both are reported.
    let mut near_dup_pairs: Vec<(&str, &str, f32)> = Vec::new();
    for (i, a) in fragments.iter().enumerate() {
        for b in fragments.iter().skip(i + 1) {
            let (Some(ea), Some(eb)) = (a.embedding.as_deref(), b.embedding.as_deref()) else {
                continue;
            };
            let sim = cosine_similarity(ea, eb);
            if sim >= RELATED_BAND {
                near_dup_pairs.push((a.content.as_str(), b.content.as_str(), sim));
            }
        }
    }

    println!("\n-- dup_rate -------------------------------------------------");
    for (a, b) in &dup_pairs {
        println!("  DUP   {a}\n        {b}");
    }
    println!(
        "  {} lexical duplicate pair(s) among {} stored rows -- dup_rate = {:.3}",
        dup_pairs.len(),
        fragments.len(),
        dup_rate
    );
    for (a, b, sim) in &near_dup_pairs {
        println!("  NEAR  {sim:.3}  {a}\n              {b}");
    }
    println!(
        "  {} stored pair(s) at or above the Related band ({RELATED_BAND}) that the write gate \n  \
         let through",
        near_dup_pairs.len()
    );
    measured.insert("dup_rate".into(), dup_rate as f64);
    measured.insert("near_dup_pairs".into(), near_dup_pairs.len() as f64);

    // ── band histogram ───────────────────────────────────────────────────
    // Same / Related / New per candidate, scored against the store at offer time.
    let mut same: Vec<&str> = Vec::new();
    let mut related: Vec<&str> = Vec::new();
    let mut fresh = 0usize;
    let mut banded = 0usize;
    for (key, sim) in &corpus.best_sim_at_offer {
        if matches!(corpus.outcomes.get(key), Some(Outcome::Refused(_))) {
            continue;
        }
        banded += 1;
        if *sim >= SAME_BAND {
            same.push(key);
        } else if *sim >= RELATED_BAND {
            related.push(key);
        } else {
            fresh += 1;
        }
    }
    println!("\n-- band histogram, candidate against the store at offer time -");
    println!(
        "  same    (>= {SAME_BAND})        {:>3}   {same:?}",
        same.len()
    );
    println!(
        "  related ([{RELATED_BAND}, {SAME_BAND}))  {:>3}   {related:?}",
        related.len()
    );
    println!("  new     (<  {RELATED_BAND})        {fresh:>3}");
    println!("  {banded} well-formed candidates banded");
    measured.insert("band_same".into(), same.len() as f64);
    measured.insert("band_related".into(), related.len() as f64);
    measured.insert("band_new".into(), fresh as f64);
    for key in same.iter().chain(related.iter()) {
        println!(
            "      {key} @ {:.3}  {:?}  {}",
            corpus.best_sim_at_offer[*key], corpus.outcomes[*key], corpus.recorded[*key]
        );
    }
    let banded_high = same.len() + related.len();
    let dropped = same
        .iter()
        .chain(related.iter())
        .filter(|k| corpus.outcomes[**k] == Outcome::Deduped)
        .count();
    println!(
        "  {dropped} of those {banded_high} candidates were DROPPED, and {} were stored \n  \
         ALONGSIDE what they restate. Both are losses of the same evidence: one throws the \n  \
         better wording away, the other keeps two rows saying one thing. Building on a match \n  \
         is the one decision that is neither.",
        banded_high - dropped
    );

    // ── recall@5 ─────────────────────────────────────────────────────────
    let qs = queries();
    let closest_floor = embedder.similarity_floors().map(|f| f.closest_memory);
    let mut hits = 0usize;
    let mut answerable = 0usize;
    println!("\n-- recall@{INJECTION_LIMIT} ---------------------------------------------");
    for q in &qs {
        let reachable: Vec<&String> = q
            .relevant
            .iter()
            .filter(|k| corpus.stored_ids.contains_key(*k))
            .collect();
        if reachable.is_empty() {
            println!(
                "  n/a  {}  (every relevant fact was refused or deduped)",
                q.query
            );
            continue;
        }
        answerable += 1;
        let pool = candidates_for(&q.query, fragments, &embedder).await;
        let pool = select_for_turn(pool, INJECTION_LIMIT, closest_floor, now);
        let top: BTreeSet<&str> = pool.iter().map(|(f, _)| f.id.as_str()).collect();
        let hit = reachable
            .iter()
            .any(|k| top.contains(corpus.stored_ids[*k].as_str()));
        if hit {
            hits += 1;
        }
        println!("  {}  {}", if hit { "HIT " } else { "miss" }, q.query);
    }
    let recall_at_5 = hits as f32 / answerable as f32;
    println!(
        "\n  {hits} of {answerable} answerable queries put an answer in the top {INJECTION_LIMIT} \
         -- recall@{INJECTION_LIMIT} = {recall_at_5:.3}"
    );
    println!(
        "  ({} of {} labelled queries are answerable at all; the rest name facts the gate refused)",
        answerable,
        qs.len()
    );
    measured.insert("recall_at_5".into(), recall_at_5 as f64);
    measured.insert("answerable_queries".into(), answerable as f64);

    // ── reachable@budget ─────────────────────────────────────────────────
    println!("\n-- reachable@budget -----------------------------------------");
    for window in [4_096usize, 8_192, 16_384] {
        let profile = CompactionProfile::from_context_window(window);
        let mut in_block = 0usize;
        let mut scored = 0usize;
        let mut widths = Vec::new();
        for q in &qs {
            let reachable: Vec<&String> = q
                .relevant
                .iter()
                .filter(|k| corpus.stored_ids.contains_key(*k))
                .collect();
            if reachable.is_empty() {
                continue;
            }
            scored += 1;
            let pool = candidates_for(&q.query, fragments, &embedder).await;
            let pool = select_for_turn(pool, profile.max_memory_fragments, closest_floor, now);
            let kept = within_budget(&pool, window);
            widths.push(kept.len());
            let ids: BTreeSet<&str> = kept.iter().map(|f| f.id.as_str()).collect();
            if reachable
                .iter()
                .any(|k| ids.contains(corpus.stored_ids[*k].as_str()))
            {
                in_block += 1;
            }
        }
        let mean_width = widths.iter().sum::<usize>() as f32 / widths.len() as f32;
        println!(
            "  window {:>6}  budget {:>4} tok / {:>2} frags  block holds {:.1} of {} rows  \
             answers {:>2} of {} ({:.3})",
            window,
            profile.memory_token_budget,
            profile.max_memory_fragments,
            mean_width,
            fragments.len(),
            in_block,
            scored,
            in_block as f32 / scored as f32,
        );
        measured.insert(
            format!("reachable_at_{window}"),
            in_block as f64 / scored as f64,
        );
    }

    println!("\n== end of baseline ==========================================\n");

    // ── the lock ─────────────────────────────────────────────────────────
    let recorded_baseline: Value = serde_json::from_str(BASELINE).expect("baseline.json");
    let expected = recorded_baseline["metrics"]
        .as_object()
        .expect("baseline.json has no metrics object");
    let mut drift: Vec<String> = Vec::new();
    println!("-- against the recorded baseline ----------------------------");
    for (name, value) in &measured {
        let want = expected
            .get(name)
            .and_then(|v| v.as_f64())
            .unwrap_or_else(|| panic!("baseline.json records no value for {name}"));
        let moved = (want - value).abs() > 1e-4;
        println!(
            "  {} {:<20} baseline {:>8.3}   now {:>8.3}",
            if moved { "MOVED" } else { "same " },
            name,
            want,
            value
        );
        if moved {
            drift.push(format!("{name}: baseline {want:.4}, now {value:.4}"));
        }
    }
    for name in expected.keys() {
        assert!(
            measured.contains_key(name),
            "baseline.json records {name}, which this run no longer measures"
        );
    }
    println!();

    // Premise guards: each firing means the harness can no longer see what it measures.
    assert!(
        recorded_with_dates > 0,
        "not one recorded fact contains a calendar date, so date_leak_rate is zero for the \
         wrong reason: the auditor has nothing to find. This is the vacuity control for the \
         assertion below it, and it is about the FIXTURES -- fix the corpus, not the gate."
    );
    // The same control per date class, so deleting one class's fixture fails the build.
    for (key, class) in [
        ("t30:0", "an ISO 8601 date, which is ONE whitespace token"),
        ("t30:1", "a slash date, likewise one token"),
        ("t31:0", "a spelled day of the month"),
        ("t31:1", "a spelled ordinal leading into a month name"),
        ("t32:0", "a spelled clock hour with a meridiem"),
        ("t32:1", "a bare spelled clock hour"),
        ("t33:1", "a decade"),
        ("t34:1", "a counted stretch of time"),
        ("t37:1", "a bare-digit clock time written as one token, 9am"),
        (
            "t38:0",
            "a day, month and year written out, where only the month is a word",
        ),
        (
            "t38:1",
            "a numbered day of the month with the unit it repeats on",
        ),
    ] {
        let recorded = corpus.recorded.get(key).unwrap_or_else(|| {
            panic!("the corpus no longer contains {key}, which is its only fact carrying {class}")
        });
        assert!(
            carries_a_date(recorded),
            "{key} is the corpus's probe for {class} and the auditor does not see a date in \
             {recorded:?}. Either the fixture was reworded or the auditor was narrowed; \
             either way date_leak_rate stops covering that class."
        );
    }
    // The reverse control: a gate that destroyed every weekday would also score zero, so these
    // recurrences must be stored whole and audited as carrying no date.
    for (key, class) in [
        ("t33:0", "a plural weekday, which cannot name one day"),
        ("t34:0", "two month names in a list, marked by \"each\""),
        ("t35:0", "a weekday marked by \"each\""),
        ("t35:1", "a weekday marked by \"every\""),
        (
            "t18:0",
            "a plural weekday carrying a standing household rule",
        ),
    ] {
        let recorded = corpus.recorded.get(key).unwrap_or_else(|| {
            panic!("the corpus no longer contains {key}, its only recurrence carrying {class}")
        });
        assert!(
            !carries_a_date(recorded),
            "{key} is {class} -- a habit, not a date -- and the auditor calls it a date in \
             {recorded:?}. That is the same conflation the stripper had, one level up: while \
             it stands, the only way to hold date_leak_rate at zero is to destroy the pattern."
        );
        assert!(
            fragments.iter().any(|f| &f.content == recorded),
            "{key} is {class} and reached the store as something other than what was said. \
             A recurrence is the fact; stripping the weekday out of it leaves a sentence that \
             asserts nothing, and {recorded:?} is not in the store."
        );
    }
    // And the not-dates: a number with a word in front of it is not a date.
    for (key, class) in [
        ("t36:0", "birth order, which is a relationship fact"),
        ("t36:1", "a blood pressure"),
        ("t37:0", "a version string with a year-shaped first part"),
    ] {
        let recorded = corpus.recorded.get(key).unwrap_or_else(|| {
            panic!("the corpus no longer contains {key}, its only fact carrying {class}")
        });
        assert!(
            !carries_a_date(recorded),
            "{key} carries {class} and the auditor reads a date in {recorded:?}"
        );
        assert!(
            fragments.iter().any(|f| &f.content == recorded),
            "{key} carries {class} and did not reach the store intact"
        );
    }
    // The detector's known false positives: these must be stored verbatim. The four after them
    // (a year or digit ordinal used as a name) are refused on purpose: the tokens match a date.
    for (key, class) in [
        ("t40:0", "a quantity after a clock lead: the oven at 180"),
        (
            "t40:1",
            "the same, INSIDE clock range and still a temperature",
        ),
        ("t41:0", "a relative-time word that is the cat's name"),
        (
            "t41:1",
            "the modal \"may\", which a word list reads as the month",
        ),
    ] {
        let recorded = corpus.recorded.get(key).unwrap_or_else(|| {
            panic!("the corpus no longer contains {key}, its only fact carrying {class}")
        });
        assert!(
            !carries_a_date(recorded),
            "{key} carries {class} and the auditor reads a date in {recorded:?}"
        );
        assert!(
            matches!(corpus.outcomes.get(key), Some(Outcome::Stored(_))),
            "{key} carries {class} and the gate did not store it. Nothing edits a note \
             now, so a refusal here is the detector claiming a sentence that holds no \
             date -- and it costs the whole fact rather than a few words of it."
        );
        assert!(
            fragments.iter().any(|f| &f.content == recorded),
            "{key} carries {class} and reached the store as something other than what was \
             said. Nothing is allowed to edit a note: the store gets the model's sentence \
             or it gets nothing."
        );
    }
    for (key, class) in [
        (
            "t39:0",
            "a year used as a modifier: \"the 2019 model of the tractor\"",
        ),
        (
            "t39:1",
            "the same, one clause over: \"the 1998 recipe book\"",
        ),
        ("t42:0", "an ordinal numeral naming a floor"),
        ("t42:1", "an ordinal numeral naming a placing in an exam"),
    ] {
        let recorded = corpus.recorded.get(key).unwrap_or_else(|| {
            panic!("the corpus no longer contains {key}, its only fact carrying {class}")
        });
        assert_eq!(
            corpus.outcomes.get(key),
            Some(&Outcome::Refused(FactDefect::CalendarDate.to_string())),
            "{key} carries {class}. It is the detector's known over-fire, and the one \
             thing it must never become again is a REWRITE: {recorded:?}"
        );
        assert!(
            !fragments.iter().any(|f| f.content == as_stored(recorded)),
            "{key} carries {class} and reached the store anyway"
        );
    }
    assert_eq!(
        measured["date_leak_rate"], 0.0,
        "a stored memory carries a calendar date. The decision is that a memory never does: \
         it is read back months later with no conversation around it, and by then the date \
         is not merely useless but wrong. The date half belongs in a proposal, which expires."
    );
    assert!(
        refused.len() >= 3,
        "the corpus must exercise more than one class of defect, or the gate is untested"
    );
    assert!(
        measured["band_same"] > 0.0 && measured["band_related"] > 0.0,
        "the Same and Related bands are empty, so the histogram that is supposed to \
         calibrate the thresholds calibrates nothing"
    );
    assert!(
        (answerable * 2) > qs.len(),
        "most of the labelled queries are unanswerable; the corpus is measuring the gate, \
         not retrieval"
    );
    assert!(
        fragments.len() > INJECTION_LIMIT * 3,
        "the store is too small for a {INJECTION_LIMIT}-slot block to be a real squeeze: \
         {} rows",
        fragments.len()
    );

    // Last, so a run that drifted still prints everything above it.
    assert!(
        drift.is_empty(),
        "{} measurement(s) moved away from the recorded baseline:\n  {}\n\n\
         This is not automatically a failure of the change -- it is the point of the \
         file. Decide whether the move is the improvement you intended, then update \
         crates/pond-infra/tests/fixtures/memory-reachability/baseline.json in the same \
         commit, with the reason in its `history`.",
        drift.len(),
        drift.join("\n  ")
    );
}
