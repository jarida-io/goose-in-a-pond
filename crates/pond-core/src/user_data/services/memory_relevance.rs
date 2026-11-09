//! Memory retrieval scoring and dedup shared by the injection path and the extraction pipeline.

use crate::user_data::domain::memory::MemoryFragment;
use chrono::{DateTime, Utc};

// ── Keyword fallback ─────────────────────────────────────────────────────────

/// Keyword-fallback stopwords; kept small since each extra entry risks dropping a topical word.
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

/// Shorter tokens are mostly function words and match too broadly via SQL `LIKE %kw%`.
const MIN_KEYWORD_LEN: usize = 3;

/// Fallback search keywords: lowercased, de-punctuated, stopword-free, deduped in first-seen order.
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
pub const IMPORTANCE_WEIGHT: f32 = 0.3;
pub const RECENCY_WEIGHT: f32 = 0.2;

/// Importance assumed for fragments written before importance was recorded.
const DEFAULT_IMPORTANCE: f32 = 0.5;

/// Short on purpose: recency is a tiebreaker, not retention (that's `memory_cleanup`'s decay).
pub const RECENCY_HALF_LIFE_DAYS: f32 = 14.0;

/// Recency in `[0, 1]` from `created_at`, not `last_accessed_at`: injection refreshes the latter,
/// so ranking on it would keep already-injected memories winning.
pub fn recency_score(fragment: &MemoryFragment, now: DateTime<Utc>) -> f32 {
    let reference = fragment.created_at;
    let days = (now - reference).num_seconds() as f32 / 86_400.0;
    if days <= 0.0 {
        return 1.0;
    }
    0.5_f32.powf(days / RECENCY_HALF_LIFE_DAYS)
}

/// Blended injection score. `None` similarity (recency-only candidates) scores 0, not neutral,
/// so a topical hit can displace a high-importance identity memory.
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

/// Sort best-first by [`relevance_score`]; ties break on id so the prompt's KV prefix is stable.
pub fn rank_by_relevance(candidates: &mut [(MemoryFragment, Option<f32>)], now: DateTime<Utc>) {
    candidates.sort_by(|a, b| {
        let sa = relevance_score(&a.0, a.1, now);
        let sb = relevance_score(&b.0, b.1, now);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
}

/// The blend's best `limit`, led by the memory closest to the message when it scores at least
/// `closest_floor`, since on a narrow scale no cosine lifts an older answer past fresh ones.
pub fn select_for_turn(
    mut candidates: Vec<(MemoryFragment, Option<f32>)>,
    limit: usize,
    closest_floor: Option<f32>,
    now: DateTime<Utc>,
) -> Vec<(MemoryFragment, Option<f32>)> {
    rank_by_relevance(&mut candidates, now);
    if let Some(floor) = closest_floor {
        // Strictly greater, so a tie goes to the better-blended memory.
        let closest = candidates
            .iter()
            .enumerate()
            .filter_map(|(i, (_, similarity))| similarity.filter(|s| s.is_finite()).map(|s| (i, s)))
            .fold(None, |best: Option<(usize, f32)>, (i, s)| match best {
                Some((_, b)) if b >= s => best,
                _ => Some((i, s)),
            });
        if let Some((i, _)) = closest.filter(|&(_, s)| s >= floor) {
            // First, because the token budget trims from the end.
            let seat = candidates.remove(i);
            candidates.insert(0, seat);
        }
    }
    candidates.truncate(limit);
    candidates
}

// ── Lexical dedup ────────────────────────────────────────────────────────────
// Sole dedup with no embedder, or one whose cosine scale is unmeasured, so it must catch
// rewordings, not just substrings.

/// Recent memories compared against a new fact; never sent to the LLM, so sized for recall.
pub const DEDUP_RECENT_WINDOW: usize = 50;

/// Jaccard floor (shared content words over all content words).
pub const LEXICAL_DEDUP_JACCARD: f32 = 0.45;

/// Containment floor (shared content words over the *shorter* side); both floors must be cleared.
pub const LEXICAL_DEDUP_CONTAINMENT: f32 = 0.8;

/// Min content words per side to trust the token measure; fewer and one word swings the ratios.
const MIN_DEDUP_TOKENS: usize = 3;

/// Dedup stopwords; unlike [`STOPWORDS`] drops "user" (in every fact) and keeps negations.
const DEDUP_STOPWORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "am", "in", "on", "at",
    "of", "to", "for", "and", "or", "but", "with", "that", "this", "these", "those", "it", "its",
    "as", "by", "from", "has", "have", "had", "do", "does", "did", "user", "he", "she", "they",
    "them", "their", "his", "her", "him", "my", "me", "mine", "our", "ours", "we", "you", "your",
    "there", "here", "then", "than", "which", "who", "what", "when", "will", "would", "can",
    "could", "should", "also", "very", "some", "into", "about", "one", "so", "if", "up", "out",
];

/// Kinship synonyms folded to one form so "mom" and "mother" facts dedup.
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

/// Shorter than [`MIN_KEYWORD_LEN`]: dedup keeps "pm"/"ai" and never runs a SQL `LIKE`.
const MIN_DEDUP_TOKEN_LEN: usize = 2;

/// Normalise like [`content_tokens`] but unfiltered, since the order markers are stopwords.
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

/// A memory's content words: normalised, singularised, alias-folded, stopword-filtered, deduped.
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

/// Drop a plural "s", except where it is part of the stem ("status", "class", "analysis").
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

/// Polarity-flipping words; checked separately because the token measure is blind to them.
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

/// Connectives whose arguments can't be swapped ("X over Y" vs "Y over X"); matched on
/// normalised words since most are dedup stopwords. No copulas: "A is B" equals "B is A".
const ORDER_SENSITIVE_MARKERS: &[&str] = &[
    "over", "than", "instead", "rather", "versus", "vs", "before", "after", "above", "below", "to",
    "from",
];

/// Raw `(jaccard, containment)` of content words; use [`is_duplicate_content`] to decide.
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

/// True when a shared [`ORDER_SENSITIVE_MARKERS`] connective has its arguments swapped.
/// Needs both crossings and ignores words on both sides, so it fires only on a real reversal.
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

/// True when two memories say the same thing, judged without embeddings. The polarity and
/// argument-swap gates run first because the token measure is blind to both.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::embedding_models::{ALL_MINILM_L6_V2, EMBEDDINGGEMMA_2};
    use crate::user_data::domain::memory::MemorySegment;
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
        // Standing identity block: max importance, recent, but unrelated to this turn.
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

    // ── a turn's five ───────────────────────────────────────────────────

    const SLOTS: usize = 5;

    fn eg2_floor() -> f32 {
        EMBEDDINGGEMMA_2.floors.expect("measured").closest_memory
    }

    fn scored(id: &str, importance: f32, age_days: i64, cosine: f32) -> Candidate {
        (fragment(id, importance, age_days), Some(cosine))
    }

    type Candidate = (MemoryFragment, Option<f32>);

    /// The nano owner's 49: "phone" answers but is 17 days old, 44th on importance and recency, and
    /// ten important day-old memories are not about it. `unrelated` spreads the other 47 cosines.
    fn household(answer: f32, nearby: f32, unrelated: (f32, f32)) -> Vec<Candidate> {
        let at = |i: usize| unrelated.0 + (unrelated.1 - unrelated.0) * i as f32 / 46.0;
        let mut store = vec![
            scored("phone", 0.8, 17, answer),
            scored("nearby", 0.6, 5, nearby),
        ];
        for i in 0..10 {
            store.push(scored(
                &format!("fresh-{i:02}"),
                0.85 + 0.01 * (i % 5) as f32,
                1,
                at(i),
            ));
        }
        for i in 0..32 {
            let importance = 0.7 + 0.006 * (i % 25) as f32;
            store.push(scored(
                &format!("week-{i:02}"),
                importance,
                2 + (i % 8) as i64,
                at(i + 10),
            ));
        }
        for i in 0..5 {
            store.push(scored(
                &format!("old-{i}"),
                0.4,
                30 + 15 * i as i64,
                at(i + 42),
            ));
        }
        store
    }

    fn ids(kept: &[Candidate]) -> Vec<&str> {
        kept.iter().map(|(m, _)| m.id.as_str()).collect()
    }

    #[test]
    fn an_older_memory_about_the_question_keeps_a_seat_among_fresh_ones() {
        let now = Utc::now();
        // EmbeddingGemma 2 at 256 dimensions: an answer scores a median 0.79 against its question,
        // a memory that is not the answer 0.61, and the nearest of those 0.76.
        let store = household(0.80, 0.76, (0.55, 0.70));

        let mut by_standing: Vec<Candidate> =
            store.iter().map(|(m, _)| (m.clone(), None)).collect();
        rank_by_relevance(&mut by_standing, now);
        let standing = by_standing.iter().position(|(m, _)| m.id == "phone");
        assert_eq!(
            standing,
            Some(43),
            "the shape: 44th of 49 on importance and recency"
        );
        let blend_alone = select_for_turn(store.clone(), SLOTS, None, now);
        assert!(
            !ids(&blend_alone).contains(&"phone"),
            "the blend alone drops the answer"
        );

        let kept = select_for_turn(store, SLOTS, Some(eg2_floor()), now);
        assert_eq!(kept.len(), SLOTS);
        assert_eq!(
            ids(&kept)[0],
            "phone",
            "the closest memory leads: {:?}",
            ids(&kept)
        );
        assert_eq!(ids(&kept)[1..], ids(&blend_alone)[..SLOTS - 1]);
    }

    #[test]
    fn a_memory_merely_near_the_question_takes_no_seat() {
        let now = Utc::now();
        // Nothing clears the floor: the closest is a near miss, as close as EG2's usually are.
        let store = household(0.74, 0.76, (0.55, 0.70));
        assert_eq!(
            ids(&select_for_turn(
                store.clone(),
                SLOTS,
                Some(eg2_floor()),
                now
            )),
            ids(&select_for_turn(store, SLOTS, None, now)),
        );
    }

    #[test]
    fn only_the_closest_memory_takes_the_seat() {
        let now = Utc::now();
        let kept = select_for_turn(
            household(0.80, 0.79, (0.55, 0.70)),
            SLOTS,
            Some(eg2_floor()),
            now,
        );
        assert_eq!(ids(&kept)[0], "phone");
        assert!(!ids(&kept).contains(&"nearby"), "{:?}", ids(&kept));
    }

    #[test]
    fn the_seat_holds_on_minilms_scale_too() {
        let now = Utc::now();
        let floor = ALL_MINILM_L6_V2.floors.expect("measured").closest_memory;
        // MiniLM spreads unrelated memories near 0.1, so the blend drops an older answer only when
        // the fresh ones sit near it, as a question's closest non-answers do (median 0.47).
        let store = household(0.58, 0.50, (0.36, 0.46));
        assert!(!ids(&select_for_turn(store.clone(), SLOTS, None, now)).contains(&"phone"));
        assert_eq!(
            ids(&select_for_turn(store, SLOTS, Some(floor), now))[0],
            "phone"
        );
    }

    #[test]
    fn a_closest_memory_already_kept_moves_to_the_front() {
        let now = Utc::now();
        let mut store = household(0.60, 0.60, (0.55, 0.70));
        store.push(scored("fresh-closest", 0.5, 1, 0.80));
        let blend_alone = select_for_turn(store.clone(), SLOTS, None, now);
        let at = ids(&blend_alone)
            .iter()
            .position(|id| *id == "fresh-closest");
        assert!(at.is_some_and(|i| i > 0), "{:?}", ids(&blend_alone));

        let kept = select_for_turn(store, SLOTS, Some(eg2_floor()), now);
        assert_eq!(ids(&kept)[0], "fresh-closest");
        let mut rest = ids(&blend_alone);
        rest.retain(|id| *id != "fresh-closest");
        assert_eq!(ids(&kept)[1..], rest[..]);
    }

    #[test]
    fn without_a_measured_floor_or_any_cosine_the_blend_decides() {
        let now = Utc::now();
        let store = household(0.80, 0.76, (0.55, 0.70));
        let mut ranked = store.clone();
        rank_by_relevance(&mut ranked, now);
        ranked.truncate(SLOTS);
        assert_eq!(ids(&select_for_turn(store, SLOTS, None, now)), ids(&ranked));

        // Keyword hits carry no cosine, so nothing can claim the seat.
        let keyword_hits: Vec<Candidate> = household(0.80, 0.76, (0.55, 0.70))
            .into_iter()
            .map(|(m, _)| (m, None))
            .collect();
        let mut ranked = keyword_hits.clone();
        rank_by_relevance(&mut ranked, now);
        ranked.truncate(SLOTS);
        assert_eq!(
            ids(&select_for_turn(
                keyword_hits,
                SLOTS,
                Some(eg2_floor()),
                now
            )),
            ids(&ranked)
        );
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
        // Reworded copies sharing no substring, so only the token measure can catch them.
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
        // Each pair shares wording but states something different.
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
        // The token measure alone calls these duplicates; only the polarity gate separates them.
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

        // A reversed journey: both cities follow "to" in each, so only "from" catches it.
        let there = "The user moved to Nairobi from Kisumu";
        let back = "The user moved to Kisumu from Nairobi";
        assert_eq!(lexical_overlap(there, back), (1.0, 1.0));
        assert!(!is_duplicate_content(there, back));
        assert!(!is_duplicate_content(back, there), "must be symmetric");
    }

    #[test]
    fn a_same_order_restatement_is_still_caught() {
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
}
