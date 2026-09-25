//! Memory relevance — retrieval-side scoring shared by the injection path and
//! the extraction pipeline.
//!
//! Three concerns live here, all pure so they stay testable in the fast-crate
//! pass:
//!
//! 1. [`keyword_terms`] — the stopword-filtered fallback used when no
//!    embedding provider is wired (or embedding the turn's message failed).
//! 2. [`relevance_score`] / [`rank_by_relevance`] — the blend of semantic
//!    similarity, importance, and recency that decides which memories survive
//!    the per-turn token budget.
//! 3. [`SEMANTIC_DEDUP_THRESHOLD`] — the cosine floor above which a newly
//!    extracted fact is considered a paraphrase of one already stored.

use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::domain::memory::MemoryFragment;
use crate::user_data::ports::memory_repository::MemoryRepository;
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

// ── Keyword fallback ─────────────────────────────────────────────────────────

/// Words carrying no retrieval signal. Kept small and English-only on purpose:
/// this list is only reached when semantic search is unavailable, and a bigger
/// list is a bigger chance of dropping a genuinely topical short word.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "your", "yours", "all", "any", "can", "had",
    "has", "have", "her", "his", "its", "our", "out", "was", "were", "who", "whom", "will", "with",
    "what", "when", "where", "which", "why", "how", "this", "that", "these", "those", "there",
    "their", "them", "then", "than", "they", "from", "into", "onto", "over", "under", "about",
    "just", "like", "some", "such", "only", "very", "much", "more", "most", "also", "been",
    "being", "does", "did", "done", "doing", "should", "would", "could", "please", "thanks",
    "thank", "let", "get", "got", "make", "made", "want", "need", "know", "tell", "say", "said",
    "one", "two", "now", "new", "old", "yes", "sure", "okay", "hey", "hi", "hello",
];

/// Minimum keyword length. Shorter tokens are almost always function words and
/// match far too broadly through a SQL `LIKE %kw%`.
const MIN_KEYWORD_LEN: usize = 3;

/// Derive fallback search keywords from a user message.
///
/// Lowercases, strips surrounding punctuation, drops tokens shorter than
/// [`MIN_KEYWORD_LEN`] and known stopwords, and de-duplicates while preserving
/// first-seen order.
pub fn keyword_terms(message: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for raw in message.split_whitespace() {
        let word = raw
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        if word.len() < MIN_KEYWORD_LEN || STOPWORDS.contains(&word.as_str()) {
            continue;
        }
        if !seen.iter().any(|w| w == &word) {
            seen.push(word);
        }
    }
    seen
}

// ── Relevance blend ──────────────────────────────────────────────────────────

/// Weight of semantic similarity in the injection score.
pub const SIMILARITY_WEIGHT: f32 = 0.5;
/// Weight of the stored importance score.
pub const IMPORTANCE_WEIGHT: f32 = 0.3;
/// Weight of the recency term.
pub const RECENCY_WEIGHT: f32 = 0.2;

/// Importance assumed for fragments written before importance was recorded.
const DEFAULT_IMPORTANCE: f32 = 0.5;

/// Half-life of the recency term, in days. A memory touched today scores 1.0;
/// one untouched for two weeks scores 0.5. Deliberately short: recency is the
/// tiebreaker between comparably relevant memories, not a retention policy
/// (that is `memory_cleanup`'s decay).
pub const RECENCY_HALF_LIFE_DAYS: f32 = 14.0;

/// Recency term in `[0, 1]` for a fragment, measured from when it was WRITTEN.
///
/// Deliberately not `last_accessed_at`. Injecting a memory refreshes that
/// timestamp, so reading from it made being injected the very thing that kept a
/// memory recent — an incumbency ratchet. On the device a memory already in the
/// prompt carried a structural head start of 0.100, meaning a genuinely more
/// relevant challenger needed a cosine edge of 0.20 just to displace it. That is
/// why "I am a computer program" and "User is an individual" kept reappearing
/// turn after turn while two real preferences never surfaced once.
///
/// `last_accessed_at` and `access_count` are still recorded and still read by
/// `memory_cleanup` for decay and reinforcement — this changes what RANKS a
/// memory for injection, not what keeps it alive.
pub fn recency_score(fragment: &MemoryFragment, now: DateTime<Utc>) -> f32 {
    let reference = fragment.created_at;
    let days = (now - reference).num_seconds() as f32 / 86_400.0;
    if days <= 0.0 {
        return 1.0;
    }
    0.5_f32.powf(days / RECENCY_HALF_LIFE_DAYS)
}

/// Blended injection score for one candidate memory.
///
/// `similarity` is `None` for candidates that arrived by recency alone; they
/// forfeit the similarity term rather than being assigned a neutral value, so a
/// topical semantic hit can displace a standing high-importance identity
/// memory instead of always losing to it.
pub fn relevance_score(
    fragment: &MemoryFragment,
    similarity: Option<f32>,
    now: DateTime<Utc>,
) -> f32 {
    let sim = similarity.unwrap_or(0.0).clamp(0.0, 1.0);
    let importance = fragment
        .importance
        .unwrap_or(DEFAULT_IMPORTANCE)
        .clamp(0.0, 1.0);
    let recency = recency_score(fragment, now);
    SIMILARITY_WEIGHT * sim + IMPORTANCE_WEIGHT * importance + RECENCY_WEIGHT * recency
}

/// Sort injection candidates best-first by [`relevance_score`].
///
/// Ties break on fragment id so the ordering (and therefore the prompt, and
/// therefore the KV prefix beyond it) is reproducible for identical inputs.
pub fn rank_by_relevance(candidates: &mut [(MemoryFragment, Option<f32>)], now: DateTime<Utc>) {
    candidates.sort_by(|a, b| {
        let sa = relevance_score(&a.0, a.1, now);
        let sb = relevance_score(&b.0, b.1, now);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
}

// ── Semantic dedup ───────────────────────────────────────────────────────────

/// Cosine floor above which a new fact is treated as a paraphrase of an
/// existing memory and dropped. Tuned high: a false skip silently loses a fact,
/// while a false keep is only a redundant row that consolidation can merge.
pub const SEMANTIC_DEDUP_THRESHOLD: f32 = 0.92;

/// How many nearest neighbours to inspect when checking for a paraphrase.
pub const SEMANTIC_DEDUP_NEIGHBOURS: usize = 5;

// ── Lexical dedup (the no-embeddings path) ──────────────────────────────────
//
// With `embedding_provider = "none"` the semantic pass above is inert: nothing
// is embedded, so nothing is ever compared. Everything below has to hold the
// line on its own, which is why it is a token measure rather than the substring
// containment it replaces — "The user's mother's name is Florence." and "My
// mom's name is Florence …" share no substring at all.

/// How many recent memories a new fact is compared against.
///
/// These strings never reach an LLM prompt (the extractor uses them only for
/// its own parse-time dedup), so the window is sized for recall, not tokens.
pub const DEDUP_RECENT_WINDOW: usize = 50;

/// Jaccard floor (shared content words over all content words).
pub const LEXICAL_DEDUP_JACCARD: f32 = 0.45;

/// Containment floor (shared content words over the *shorter* side).
///
/// Both floors must be cleared. Jaccard alone misses a short restatement of a
/// long fact; containment alone fires on "prefers dark mode" vs "prefers dark
/// roast coffee". Together they caught every duplicate pair seen in a real
/// store without merging a genuinely distinct one.
pub const LEXICAL_DEDUP_CONTAINMENT: f32 = 0.8;

/// Fewest content words either side must have before the token measure is
/// trusted. Below this a single shared word swings the ratios wildly, and
/// negation ("is happy" / "is not happy") reduces to the same token set.
const MIN_DEDUP_TOKENS: usize = 3;

/// Words carrying no *discriminative* signal between two memories. Distinct
/// from [`STOPWORDS`]: "user" is dropped here because every third-person fact
/// contains it, and negations are deliberately kept because dropping them would
/// make a fact and its contradiction look identical.
const DEDUP_STOPWORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "am", "in", "on", "at",
    "of", "to", "for", "and", "or", "but", "with", "that", "this", "these", "those", "it", "its",
    "as", "by", "from", "has", "have", "had", "do", "does", "did", "user", "he", "she", "they",
    "them", "their", "his", "her", "him", "my", "me", "mine", "our", "ours", "we", "you", "your",
    "there", "here", "then", "than", "which", "who", "what", "when", "will", "would", "can",
    "could", "should", "also", "very", "some", "into", "about", "one", "so", "if", "up", "out",
];

/// Kinship synonyms folded to one form. Without this the single most common
/// duplicate in a personal store — "mother" written once as "mom" — reads as
/// two unrelated facts.
const TOKEN_ALIASES: &[(&str, &str)] = &[
    ("mom", "mother"),
    ("mum", "mother"),
    ("mommy", "mother"),
    ("mummy", "mother"),
    ("mama", "mother"),
    ("momma", "mother"),
    ("dad", "father"),
    ("daddy", "father"),
    ("papa", "father"),
    ("grandma", "grandmother"),
    ("granny", "grandmother"),
    ("nana", "grandmother"),
    ("grandpa", "grandfather"),
    ("grandad", "grandfather"),
    ("granddad", "grandfather"),
    ("kid", "child"),
    ("children", "child"),
];

/// Shortest token kept. Two characters, not [`MIN_KEYWORD_LEN`]: dedup wants
/// every scrap of signal ("pm", "ai"), and it never runs a SQL `LIKE`.
const MIN_DEDUP_TOKEN_LEN: usize = 2;

/// Normalise one raw word the way [`content_tokens`] does, but without the
/// stopword and length filters — [`ORDER_SENSITIVE_MARKERS`] are themselves
/// stopwords, so the order check needs the unfiltered sequence.
fn normalise_word(raw: &str) -> String {
    let word = raw
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    let word = singularise(strip_possessive(&word));
    TOKEN_ALIASES
        .iter()
        .find(|(from, _)| *from == word)
        .map(|(_, to)| (*to).to_string())
        .unwrap_or(word)
}

/// Content words of a memory: lowercased, de-punctuated, possessive-stripped,
/// crudely singularised, alias-folded, stopword-filtered, order-independent.
pub fn content_tokens(text: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for raw in text.split_whitespace() {
        let word = normalise_word(raw);
        if word.len() < MIN_DEDUP_TOKEN_LEN || DEDUP_STOPWORDS.contains(&word.as_str()) {
            continue;
        }
        if !seen.iter().any(|w| w == &word) {
            seen.push(word);
        }
    }
    seen
}

fn strip_possessive(word: &str) -> &str {
    word.strip_suffix("'s")
        .or_else(|| word.strip_suffix("\u{2019}s"))
        .unwrap_or(word)
}

/// Drop a plural "s". Skips endings where the "s" is part of the stem
/// ("status", "class", "analysis") rather than a suffix.
fn singularise(word: &str) -> String {
    let keep = word.len() <= 3
        || !word.ends_with('s')
        || word.ends_with("ss")
        || word.ends_with("us")
        || word.ends_with("is")
        || word.ends_with("as");
    if keep {
        word.to_string()
    } else {
        word[..word.len() - 1].to_string()
    }
}

/// Words that flip a fact's polarity. A token measure is order- and
/// polarity-blind, so "is happy" and "is not happy" reduce to nearly the same
/// set — these are checked separately and never dropped as stopwords.
const NEGATIONS: &[&str] = &[
    "not",
    "no",
    "never",
    "nor",
    "without",
    "cannot",
    "can't",
    "don't",
    "doesn't",
    "didn't",
    "isn't",
    "aren't",
    "wasn't",
    "won't",
    "shouldn't",
    "wouldn't",
];

/// Connectives whose two arguments are not interchangeable. "X over Y" and
/// "Y over X" are opposite claims that reduce to one token set, so a set
/// measure scores the reversal a perfect duplicate — the failure that silently
/// dropped a correction and kept the stale row it was fixing.
///
/// Read as *normalised words*, not content tokens: most of these are dedup
/// stopwords ("to", "than") and would otherwise be filtered away before the
/// comparison. Copulas are deliberately absent — "Florence is the user's
/// mother" and "The user's mother is Florence" are the same fact.
const ORDER_SENSITIVE_MARKERS: &[&str] = &[
    "over", "than", "instead", "rather", "versus", "vs", "before", "after", "above", "below", "to",
    "from",
];

/// Jaccard and containment of two memories' content words, in that order.
///
/// A raw measure: it does not consider polarity, and returns `(0.0, 0.0)` when
/// either side has fewer than three content words. Use [`is_duplicate_content`]
/// to decide anything.
pub fn lexical_overlap(a: &str, b: &str) -> (f32, f32) {
    token_overlap(&content_tokens(a), &content_tokens(b))
}

fn token_overlap(ta: &[String], tb: &[String]) -> (f32, f32) {
    if ta.len() < MIN_DEDUP_TOKENS || tb.len() < MIN_DEDUP_TOKENS {
        return (0.0, 0.0);
    }
    let shared = ta.iter().filter(|t| tb.contains(t)).count() as f32;
    let union = (ta.len() + tb.len()) as f32 - shared;
    let shorter = ta.len().min(tb.len()) as f32;
    (shared / union, shared / shorter)
}

fn is_negated(tokens: &[String]) -> bool {
    tokens.iter().any(|t| NEGATIONS.contains(&t.as_str()))
}

/// Words of `text` normalised but not stopword-filtered, so a marker survives.
fn normalised_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(normalise_word)
        .filter(|w| !w.is_empty())
        .collect()
}

/// Keep only the discriminative words of one side of a marker.
fn content_of(words: &[String]) -> Vec<&str> {
    words
        .iter()
        .map(String::as_str)
        .filter(|w| w.len() >= MIN_DEDUP_TOKEN_LEN && !DEDUP_STOPWORDS.contains(w))
        .collect()
}

/// True when the two texts share an [`ORDER_SENSITIVE_MARKERS`] connective and
/// have exchanged its arguments — "prefers dark mode over light mode" against
/// "prefers light mode over dark mode".
///
/// Both directions of the crossing are required, and a word present on both
/// sides in the other text ("mode") is ignored, so this only fires on a genuine
/// reversal. Firing wrongly costs one redundant row; not firing costs a fact.
fn is_argument_swap(a: &str, b: &str) -> bool {
    let (wa, wb) = (normalised_words(a), normalised_words(b));
    for marker in ORDER_SENSITIVE_MARKERS {
        let (Some(ia), Some(ib)) = (
            wa.iter().position(|w| w == marker),
            wb.iter().position(|w| w == marker),
        ) else {
            continue;
        };
        let (a_before, a_after) = (content_of(&wa[..ia]), content_of(&wa[ia + 1..]));
        let (b_before, b_after) = (content_of(&wb[..ib]), content_of(&wb[ib + 1..]));
        let crossed_back = a_before
            .iter()
            .any(|t| b_after.contains(t) && !b_before.contains(t));
        let crossed_forward = a_after
            .iter()
            .any(|t| b_before.contains(t) && !b_after.contains(t));
        if crossed_back && crossed_forward {
            return true;
        }
    }
    false
}

/// True when two memories say the same thing, judged without embeddings.
///
/// Opposite polarity is never a duplicate, nor is an argument reversal — both
/// checked first because the token measure is blind to polarity *and* to order.
/// Then case-insensitive substring containment (the cheap exact-restatement
/// case), then the token measure for reworded duplicates.
pub fn is_duplicate_content(a: &str, b: &str) -> bool {
    let (la, lb) = (a.trim().to_lowercase(), b.trim().to_lowercase());
    if la.is_empty() || lb.is_empty() {
        return false;
    }
    let (ta, tb) = (content_tokens(&la), content_tokens(&lb));
    if is_negated(&ta) != is_negated(&tb) {
        return false;
    }
    if is_argument_swap(&la, &lb) {
        return false;
    }
    if la.contains(&lb) || lb.contains(&la) {
        return true;
    }
    let (jaccard, containment) = token_overlap(&ta, &tb);
    jaccard >= LEXICAL_DEDUP_JACCARD && containment >= LEXICAL_DEDUP_CONTAINMENT
}

// ── Embedding backfill ───────────────────────────────────────────────────────

/// Rows embedded per backfill batch.
///
/// Also bounds how long a concurrent per-turn retrieval embed can queue behind
/// the backfill: the fastembed adapter serialises on one model mutex, so a turn
/// arriving mid-batch waits for at most this many short embeds.
pub const BACKFILL_BATCH_SIZE: usize = 32;

/// Pause between backfill batches. The backfill competes with inference for CPU
/// on a Jetson, so it yields between batches rather than running flat out.
pub const BACKFILL_BATCH_PAUSE_MS: u64 = 250;

/// Embed every active memory that has no stored embedding yet, in batches.
///
/// Extraction historically stored `embedding: None`, so without this pass
/// `search_similar` sees only the handful of rows written by the `save_memory`
/// MCP tool and silently ignores the rest of the store.
///
/// Best-effort throughout: a row that fails to embed is left for the next run.
/// Returns the number of rows embedded.
pub async fn run_backfill(
    repo: &dyn MemoryRepository,
    embedder: &dyn EmbeddingProvider,
    batch_size: usize,
    pause_ms: u64,
) -> usize {
    embed_in_batches(
        repo,
        embedder,
        batch_size,
        pause_ms,
        "memory-backfill",
        None,
        None,
        usize::MAX,
    )
    .await
}

/// The RECURRING repair of the same defect [`run_backfill`] fixes once at boot.
///
/// `run_backfill` is spawned a single time per process, so every unembedded row
/// minted afterwards stays invisible to `search_similar` until the next restart
/// — and three ordinary paths mint them: consolidation's `apply_actions` writes
/// `embedding: None`, `update_content` nulls the vector on purpose (its own
/// comment promises a "next sweep" that until now did not exist), and any embed
/// that simply failed. The index sweep did not cover it either: adoption only
/// COPIES vectors that already exist.
///
/// Same batch size and pause as the backfill, because it competes with
/// inference for the same CPU. Two things the boot-time pass does not need and
/// a scheduled one does: a cancellation token, so a member coming back takes
/// the machine straight back, and `max_batches`, so a background tick takes one
/// bite of a long backlog instead of holding the lane slot until it drains.
pub async fn run_memory_embedding_sweep(
    repo: &dyn MemoryRepository,
    embedder: &dyn EmbeddingProvider,
    batch_size: usize,
    pause_ms: u64,
    cancel: &CancellationToken,
    max_batches: usize,
) -> usize {
    embed_in_batches(
        repo,
        embedder,
        batch_size,
        pause_ms,
        "memory-sweep",
        None,
        Some(cancel),
        max_batches,
    )
    .await
}

/// Re-embed every active memory whose stored vector is the WRONG WIDTH — i.e.
/// produced by a different embedding model.
///
/// [`run_backfill`] cannot reach these, because it selects `embedding IS NULL`
/// and a stale vector is not null. Without this pass, a pond that switched
/// `embedding_provider` keeps rows that semantic search correctly EXCLUDES (they
/// are not comparable) and that nothing ever repairs — retrieval quietly and
/// permanently worse, with the store looking fully embedded.
///
/// Same batching and pause as the backfill, and for the same reason: on a Jetson
/// this competes with inference for CPU. Best-effort; returns rows re-embedded.
pub async fn run_dimension_repair(
    repo: &dyn MemoryRepository,
    embedder: &dyn EmbeddingProvider,
    batch_size: usize,
    pause_ms: u64,
) -> usize {
    let expected = embedder.dimensions();
    if expected == 0 {
        tracing::warn!("[memory-reembed] provider reports 0 dimensions — skipping");
        return 0;
    }
    embed_in_batches(
        repo,
        embedder,
        batch_size,
        pause_ms,
        "memory-reembed",
        Some(expected),
        None,
        usize::MAX,
    )
    .await
}

/// The shared batching loop. `stale_dims` selects which rows are fetched: `None`
/// means "never embedded", `Some(d)` means "embedded at some width other than d".
///
/// `cancel` is `None` for the two boot-time passes, which own the process's idle
/// moment and have nothing to yield to; `max_batches` bounds a scheduled caller
/// to one bite. Both are inert for those callers rather than absent, so there is
/// one loop to reason about and not two.
#[allow(clippy::too_many_arguments)]
async fn embed_in_batches(
    repo: &dyn MemoryRepository,
    embedder: &dyn EmbeddingProvider,
    batch_size: usize,
    pause_ms: u64,
    label: &str,
    stale_dims: Option<usize>,
    cancel: Option<&CancellationToken>,
    max_batches: usize,
) -> usize {
    let cancelled = || cancel.is_some_and(|c| c.is_cancelled());
    let mut embedded = 0usize;
    let mut batches = 0usize;
    loop {
        if cancelled() || batches >= max_batches {
            break;
        }
        batches += 1;
        let fetched = match stale_dims {
            Some(dims) => repo.search_stale_dimension(dims, batch_size).await,
            None => repo.search_unembedded(batch_size).await,
        };
        let batch = match fetched {
            Ok(rows) if rows.is_empty() => break,
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("[{label}] fetch failed: {e}");
                break;
            }
        };

        let mut progressed = false;
        for fragment in &batch {
            if cancelled() {
                break;
            }
            match embedder.embed(&fragment.content).await {
                Ok(vector) => match repo.update_embedding(&fragment.id, &vector).await {
                    Ok(()) => {
                        embedded += 1;
                        progressed = true;
                    }
                    Err(e) => {
                        tracing::warn!("[{label}] store failed for {}: {e}", fragment.id)
                    }
                },
                Err(e) => tracing::warn!("[{label}] embed failed for {}: {e}", fragment.id),
            }
        }

        // Every row in the batch failed, so the same rows would come back
        // forever — stop instead of spinning.
        if !progressed {
            tracing::warn!("[{label}] no progress in a batch — stopping");
            break;
        }

        // Before the pause, not after it: a caller allowed one batch would
        // otherwise sleep a quarter-second holding the lane slot for nothing.
        if batches >= max_batches || cancelled() {
            break;
        }

        if pause_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(pause_ms)).await;
        }
    }

    if embedded > 0 {
        tracing::info!("[{label}] embedded {embedded} memories");
    }
    embedded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::memory::{MemoryLifecycle, MemorySegment};
    use crate::user_data::domain::profile::ProfileScope;
    use crate::user_data::mocks::mock_memory::MockMemoryRepository;
    use chrono::Duration;

    fn fragment(id: &str, importance: f32, age_days: i64) -> MemoryFragment {
        let mut f = MemoryFragment::from_extraction(
            id.to_string(),
            None,
            format!("content of {id}"),
            MemorySegment::Knowledge,
            importance,
            None,
        );
        f.created_at = Utc::now() - Duration::days(age_days);
        f
    }

    // ── keyword fallback ────────────────────────────────────────────────

    #[test]
    fn keyword_terms_drops_stopwords_and_short_tokens() {
        let terms = keyword_terms("What is the status of my greenhouse irrigation pump?");
        assert!(!terms.contains(&"the".to_string()));
        assert!(!terms.contains(&"what".to_string()));
        assert!(!terms.contains(&"is".to_string()));
        assert!(!terms.contains(&"my".to_string()));
        assert!(terms.contains(&"greenhouse".to_string()));
        assert!(terms.contains(&"irrigation".to_string()));
        assert!(terms.contains(&"pump".to_string()));
        assert!(terms.contains(&"status".to_string()));
    }

    #[test]
    fn keyword_terms_strips_punctuation_and_dedupes() {
        let terms = keyword_terms("Pump, pump... PUMP!");
        assert_eq!(terms, vec!["pump".to_string()]);
    }

    #[test]
    fn keyword_terms_can_return_nothing_for_a_pure_stopword_message() {
        assert!(keyword_terms("and then you know, they said what?").is_empty());
    }

    // ── ranking blend ───────────────────────────────────────────────────

    #[test]
    fn recency_score_halves_every_half_life() {
        let now = Utc::now();
        let fresh = fragment("fresh", 0.5, 0);
        let two_weeks = fragment("two-weeks", 0.5, RECENCY_HALF_LIFE_DAYS as i64);
        assert!((recency_score(&fresh, now) - 1.0).abs() < 0.01);
        assert!((recency_score(&two_weeks, now) - 0.5).abs() < 0.02);
    }

    /// Being injected must NOT make a memory look recent.
    ///
    /// Injection refreshes `last_accessed_at`, so ranking on it made the act of
    /// being chosen the reason to be chosen again — an incumbency ratchet worth
    /// 0.100 of head start, which on the device kept "I am a computer program"
    /// in the prompt while two real preferences never surfaced. Recency is now
    /// measured from when the memory was written, full stop.
    #[test]
    fn being_accessed_does_not_refresh_recency() {
        let now = Utc::now();
        let mut old_but_used = fragment("used", 0.5, 90);
        old_but_used.last_accessed_at = Some(now);
        // 90 days at a 14-day half-life is ~0.012 — the access must not rescue it.
        let score = recency_score(&old_but_used, now);
        assert!(
            score < 0.05,
            "expected the write date to govern, got {score}"
        );
    }

    #[test]
    fn topical_similarity_outranks_standing_identity_memory() {
        let now = Utc::now();
        // The standing identity block: maximum importance, recent, but no
        // semantic relationship to this turn.
        let mut identity = fragment("identity", 1.0, 0);
        identity.segment = Some(MemorySegment::Identity);
        // An old, middling-importance memory that is actually about the topic.
        let topical = fragment("topical", 0.5, 60);

        let mut candidates = vec![(identity, None), (topical, Some(0.88))];
        rank_by_relevance(&mut candidates, now);
        assert_eq!(candidates[0].0.id, "topical");
    }

    #[test]
    fn importance_still_decides_when_similarity_is_equal() {
        let now = Utc::now();
        let low = fragment("low", 0.2, 0);
        let high = fragment("high", 0.9, 0);
        let mut candidates = vec![(low, Some(0.5)), (high, Some(0.5))];
        rank_by_relevance(&mut candidates, now);
        assert_eq!(candidates[0].0.id, "high");
    }

    #[test]
    fn ranking_is_deterministic_for_identical_scores() {
        let now = Utc::now();
        let a = fragment("aaa", 0.5, 0);
        let b = fragment("bbb", 0.5, 0);
        let mut forward = vec![(a.clone(), Some(0.4)), (b.clone(), Some(0.4))];
        let mut reversed = vec![(b, Some(0.4)), (a, Some(0.4))];
        rank_by_relevance(&mut forward, now);
        rank_by_relevance(&mut reversed, now);
        assert_eq!(forward[0].0.id, "aaa");
        assert_eq!(reversed[0].0.id, "aaa");
    }

    #[test]
    fn missing_similarity_scores_as_zero_not_as_neutral() {
        let now = Utc::now();
        let f = fragment("f", 0.6, 0);
        let without = relevance_score(&f, None, now);
        let with_zero = relevance_score(&f, Some(0.0), now);
        assert!((without - with_zero).abs() < f32::EPSILON);
    }

    // ── lexical dedup ───────────────────────────────────────────────────

    #[test]
    fn content_tokens_normalises_possessives_plurals_and_kinship() {
        assert_eq!(
            content_tokens("The user's mother's name is Florence."),
            vec!["mother", "name", "florence"]
        );
        // "mom" folds onto "mother", "lives" onto "live".
        assert_eq!(
            content_tokens("My mom's name is Florence and she lives in the latter city"),
            vec!["mother", "name", "florence", "live", "latter", "city"]
        );
    }

    #[test]
    fn content_tokens_keeps_stems_that_merely_end_in_s() {
        for word in ["status", "class", "analysis", "gas", "canvas"] {
            assert_eq!(content_tokens(word), vec![word.to_string()], "{word}");
        }
    }

    #[test]
    fn the_three_florence_memories_collapse_without_embeddings() {
        // Two rows that shipped side by side in a real store: reworded copies
        // of one fact, sharing no substring, so only the token measure sees it.
        let stored = "The user's mother's name is Florence.";
        let reworded = "My mom's name is Florence and she lives in the latter city";
        assert!(is_duplicate_content(reworded, stored));
        assert!(is_duplicate_content(stored, reworded), "must be symmetric");
    }

    #[test]
    fn a_restatement_of_a_captured_task_is_a_duplicate() {
        assert!(is_duplicate_content(
            "The user waters the plants every evening at 6 PM",
            "Set a reminder to water the plants every evening at 6 PM",
        ));
    }

    #[test]
    fn distinct_facts_are_not_deduplicated() {
        // Each pair shares wording but states something different. Losing the
        // second one is the failure mode this threshold pair guards against.
        let distinct: &[(&str, &str)] = &[
            (
                "The user prefers dark mode",
                "The user prefers dark roast coffee",
            ),
            (
                "The user's mother's name is Florence.",
                "The user's father's name is Peter.",
            ),
            (
                "The user's mother lives in Kisumu",
                "The user's mother lives in Nairobi",
            ),
            ("The user's dog is named Rex", "The user's cat is named Rex"),
            (
                "The user has expressed a preference for brevity in interactions",
                "The user set an interaction preference to use only one word for a goodbye",
            ),
            (
                "The user is building a smart-home dashboard",
                "The user is reading a book about beekeeping",
            ),
        ];
        for (a, b) in distinct {
            assert!(!is_duplicate_content(a, b), "wrongly merged {a:?} / {b:?}");
        }
    }

    #[test]
    fn a_negation_is_not_a_duplicate_of_what_it_negates() {
        // The token measure does run here and scores these a duplicate
        // (Jaccard 0.75, containment 1.00) — "not" is the only token that
        // differs. The polarity gate is the only thing keeping them apart.
        let (jaccard, containment) = lexical_overlap(
            "The user is not happy with the new voice",
            "The user is happy with the new voice",
        );
        assert!(jaccard >= LEXICAL_DEDUP_JACCARD && containment >= LEXICAL_DEDUP_CONTAINMENT);
        assert!(!is_duplicate_content(
            "The user is not happy with the new voice",
            "The user is happy with the new voice"
        ));
    }

    #[test]
    fn an_argument_reversal_is_not_a_duplicate() {
        // The regression this guard exists for: the reversal reduces to the
        // *identical* token set, so Jaccard and containment both read 1.00 and
        // the correction was dropped in favour of the stale row it fixed.
        let stale = "The user prefers dark mode over light mode";
        let fixed = "The user prefers light mode over dark mode";
        assert_eq!(lexical_overlap(stale, fixed), (1.0, 1.0));
        assert!(!is_duplicate_content(stale, fixed));
        assert!(!is_duplicate_content(fixed, stale), "must be symmetric");

        // The same shape with an elided marker ("prefers X to Y").
        assert!(!is_duplicate_content(
            "The user prefers tea to coffee",
            "The user prefers coffee to tea"
        ));

        // A reversed journey. "to" alone cannot see it — both sentences put the
        // same city after "to" — so "from" has to be a marker as well.
        let there = "The user moved to Nairobi from Kisumu";
        let back = "The user moved to Kisumu from Nairobi";
        assert_eq!(lexical_overlap(there, back), (1.0, 1.0));
        assert!(!is_duplicate_content(there, back));
        assert!(!is_duplicate_content(back, there), "must be symmetric");
    }

    #[test]
    fn a_same_order_restatement_is_still_caught() {
        // The order guard must not blunt the measure: same claim, same order,
        // marker present in both.
        assert!(is_duplicate_content(
            "The user prefers dark mode over light mode",
            "The user prefers dark mode over light mode in every app",
        ));
        assert!(is_duplicate_content(
            "The user moved from Nairobi to Kisumu",
            "The user moved to Kisumu from Nairobi",
        ));
    }

    #[test]
    fn exact_and_contained_restatements_still_dedup() {
        assert!(is_duplicate_content(
            "The user works at Jarida",
            "the user works at jarida"
        ));
        assert!(is_duplicate_content(
            "The user works at Jarida as an engineer",
            "The user works at Jarida"
        ));
    }

    #[test]
    fn empty_content_is_never_a_duplicate() {
        assert!(!is_duplicate_content("", "The user works at Jarida"));
        assert!(!is_duplicate_content("   ", ""));
    }

    // ── backfill ────────────────────────────────────────────────────────

    /// Embedding provider returning a fixed non-zero vector, so backfilled rows
    /// are distinguishable from the all-zero mock.
    struct FixedEmbedder(Vec<f32>);

    #[async_trait::async_trait]
    impl EmbeddingProvider for FixedEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(self.0.clone())
        }
        fn dimensions(&self) -> usize {
            self.0.len()
        }
    }

    #[tokio::test]
    async fn backfill_embeds_every_unembedded_active_row() {
        let repo = MockMemoryRepository::new();
        for i in 0..5 {
            repo.add(fragment(&format!("m{i}"), 0.5, 0)).await.unwrap();
        }
        let mut already = fragment("already", 0.5, 0);
        already.embedding = Some(vec![9.0, 9.0]);
        repo.add(already).await.unwrap();

        let embedder = FixedEmbedder(vec![1.0, 0.0]);
        let count = run_backfill(&repo, &embedder, 2, 0).await;

        assert_eq!(count, 5);
        assert!(repo.search_unembedded(10).await.unwrap().is_empty());
        // The pre-embedded row is left exactly as it was.
        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        let untouched = stored.iter().find(|f| f.id == "already").unwrap();
        assert_eq!(untouched.embedding, Some(vec![9.0, 9.0]));
    }

    #[tokio::test]
    async fn backfill_skips_archived_rows() {
        let repo = MockMemoryRepository::new();
        let mut archived = fragment("archived", 0.5, 0);
        archived.lifecycle = Some(MemoryLifecycle::Archived);
        repo.add(archived).await.unwrap();
        repo.add(fragment("active", 0.5, 0)).await.unwrap();

        let embedded = run_backfill(&repo, &FixedEmbedder(vec![1.0, 0.0]), 8, 0).await;
        assert_eq!(embedded, 1);
    }

    #[tokio::test]
    async fn backfill_on_an_empty_store_is_a_no_op() {
        let repo = MockMemoryRepository::new();
        assert_eq!(
            run_backfill(&repo, &FixedEmbedder(vec![1.0]), 8, 0).await,
            0
        );
    }
}
