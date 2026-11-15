//! One retrieval over every corpus, scoped in [`VectorIndex`]'s SQL.
//! Each hit carries its corpus: a summary is lossy, not a said fact.

use std::sync::Arc;

use anyhow::Result;

use crate::context::chunking;
use crate::context::retrieval::CONTEXT_BLOCK_TAG;
use crate::context::vector_index::{Corpus, Exclusions, ResolvedHit, VectorIndex};
use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::domain::profile::ProfileScope;

/// How a hit is introduced to the model and member: prose, not the enum's name.
pub fn provenance(corpus: Corpus) -> &'static str {
    match corpus {
        Corpus::Memory => "you told me",
        Corpus::Context => "observed by this pond",
        Corpus::Summary => "from an earlier conversation",
        Corpus::Personal => "from your profile",
        Corpus::Message => "said in an earlier conversation",
        Corpus::Digest => "from a news digest I wrote",
        Corpus::Story => "from a news story I picked for you",
        Corpus::Report => "from a long-task report",
        Corpus::Notice => "from a notification I sent",
        Corpus::Idea => "from an idea I suggested",
    }
}

/// A hit's provenance, told apart within personal details by the table its id names.
pub fn label(corpus: Corpus, row_id: &str) -> &'static str {
    if corpus != Corpus::Personal {
        return provenance(corpus);
    }
    match row_id.split_once(':').map(|(kind, _)| kind) {
        Some("interest") => "from your interests",
        Some("topic") => "from the news topics I keep for you",
        Some("portrait") => "from what I have pieced together about you",
        Some("saved") => "something you saved",
        Some("screenshot") => "from a screenshot you shared",
        _ => provenance(corpus),
    }
}

/// Hits a turn carries at most; a choice, so a strong match never crowds out the message.
pub const TURN_HITS: usize = 3;

/// One line's longest text before the budget applies; about two sentences of a passage.
const TURN_LINE_CHARS: usize = 480;

/// The least of the first hit's text kept however tight the budget, so a cut line still says
/// something.
const TURN_LINE_MIN_CHARS: usize = 24;

/// The turn's share of `hits`: those at or above `floor`, personal details first, then the
/// closest, at most `limit`.
pub fn for_turn(mut hits: Vec<Recollection>, floor: f32, limit: usize) -> Vec<Recollection> {
    hits.retain(|hit| hit.score.is_finite() && hit.score >= floor);
    hits.sort_by(|a, b| {
        (b.corpus == Corpus::Personal)
            .cmp(&(a.corpus == Corpus::Personal))
            .then(b.score.total_cmp(&a.score))
            .then_with(|| a.corpus.cmp(&b.corpus))
            .then_with(|| a.row_id.cmp(&b.row_id))
    });
    hits.truncate(limit);
    hits
}

/// One hit as a block line: its provenance, then its text on one line, cut to `max_chars` at a
/// word break.
pub fn turn_line(hit: &Recollection, max_chars: usize) -> String {
    let text = hit.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = match chunking::cut_at_word(&text, max_chars) {
        Some(head) => format!("{head}…"),
        None => text,
    };
    format!("- ({}) {text}", label(hit.corpus, &hit.row_id))
}

/// The `<personal-context>` block for `hits` within `token_budget` (chars/4, as memories are
/// counted): the first line is always kept, cut to fit; later ones only whole. Empty for none.
/// Also returns how many hits it holds.
pub fn render_turn_block(hits: &[Recollection], token_budget: usize) -> (String, usize) {
    let budget_chars = token_budget.saturating_mul(4);
    let mut used = 0usize;
    let mut lines: Vec<String> = Vec::new();
    for hit in hits {
        let room = budget_chars.saturating_sub(used);
        let line = if lines.is_empty() {
            let prefix = label(hit.corpus, &hit.row_id).chars().count() + "- () ".len();
            let text_room = room.saturating_sub(prefix).max(TURN_LINE_MIN_CHARS);
            turn_line(hit, TURN_LINE_CHARS.min(text_room))
        } else {
            turn_line(hit, TURN_LINE_CHARS)
        };
        if !lines.is_empty() && line.chars().count() > room {
            break;
        }
        used += line.chars().count() + 1;
        lines.push(line);
    }
    if lines.is_empty() {
        return (String::new(), 0);
    }
    let block = format!(
        "<{CONTEXT_BLOCK_TAG}>\n{}\n</{CONTEXT_BLOCK_TAG}>",
        lines.join("\n")
    );
    (block, lines.len())
}

/// A retrieval answer: the text, where it came from, and how well it matched.
#[derive(Debug, Clone, PartialEq)]
pub struct Recollection {
    pub corpus: Corpus,
    pub row_id: String,
    pub score: f32,
    pub text: String,
}

impl Recollection {
    /// The hit, introduced by its provenance.
    pub fn labelled(&self) -> String {
        format!("({}) {}", label(self.corpus, &self.row_id), self.text)
    }
}

/// Unified retrieval over the personal-context index.
pub struct PersonalContextRetrieval {
    index: Arc<dyn VectorIndex>,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl PersonalContextRetrieval {
    pub fn new(index: Arc<dyn VectorIndex>, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self { index, embedder }
    }

    /// Recall up to `limit` things relevant to `query`, within `scope`.
    /// Errors log and return empty, never failing the turn; the caller falls back to keywords.
    pub async fn recall(
        &self,
        query: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Vec<Recollection> {
        if query.trim().is_empty() || limit == 0 || scope.excludes_everything() {
            return vec![];
        }
        match self.try_recall(query, scope, limit).await {
            Ok(hits) => hits,
            Err(e) => {
                tracing::warn!("personal-context recall failed, answering empty: {e:#}");
                vec![]
            }
        }
    }

    /// [`Self::recall`] for a query already embedded, as a turn's memory search embeds it.
    pub async fn recall_near(
        &self,
        query_vector: &[f32],
        scope: &ProfileScope,
        limit: usize,
        exclusions: &Exclusions,
    ) -> Vec<Recollection> {
        if query_vector.is_empty() || limit == 0 || scope.excludes_everything() {
            return vec![];
        }
        let model_id = self.embedder.model_id();
        match self
            .index
            .search_resolved_excluding(query_vector, &model_id, scope, limit, exclusions)
            .await
        {
            Ok(hits) => recollections(hits),
            Err(e) => {
                tracing::warn!("personal-context recall for a turn failed: {e:#}");
                vec![]
            }
        }
    }

    async fn try_recall(
        &self,
        query: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<Recollection>> {
        // `embed_query`, not `embed`: asymmetric retrievers rank worse with the document form.
        let vector = self.embedder.embed_query(query).await?;
        let model_id = self.embedder.model_id();

        // Scope is applied in SQL, not after: post-filtering lets invisible rows take top-K slots.
        let hits: Vec<ResolvedHit> = self
            .index
            .search_resolved(&vector, &model_id, scope, limit)
            .await?;

        Ok(recollections(hits))
    }
}

fn recollections(hits: Vec<ResolvedHit>) -> Vec<Recollection> {
    hits.into_iter()
        .filter(|h| !h.text.trim().is_empty())
        .map(|h| Recollection {
            corpus: h.corpus,
            row_id: h.row_id,
            score: h.score,
            text: h.text,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::vector_index::{
        CorpusHealth, Exclusions, IndexHealth, StaleRow, VectorEntry, VectorHit,
    };
    use async_trait::async_trait;

    /// The tie-break is `Corpus`'s declaration order; reordering variants changes it.
    #[test]
    fn memory_wins_ties_over_context_and_summary() {
        assert!(Corpus::Memory < Corpus::Context);
        assert!(Corpus::Context < Corpus::Summary);
        let mut all = vec![Corpus::Summary, Corpus::Memory, Corpus::Context];
        all.sort();
        assert_eq!(
            all,
            vec![Corpus::Memory, Corpus::Context, Corpus::Summary],
            "the tie-break order changed: a summary could now outrank a memory"
        );
    }

    #[test]
    fn every_corpus_states_its_provenance_in_words() {
        for c in Corpus::ALL {
            let p = provenance(c);
            assert!(!p.is_empty());
            assert_ne!(p, c.as_str());
        }
        let r = Recollection {
            corpus: Corpus::Summary,
            row_id: "s1".into(),
            score: 0.9,
            text: "we agreed on Tuesday".into(),
        };
        assert_eq!(
            r.labelled(),
            "(from an earlier conversation) we agreed on Tuesday"
        );
    }

    fn recollection(corpus: Corpus, id: &str, score: f32, text: &str) -> Recollection {
        Recollection {
            corpus,
            row_id: id.into(),
            score,
            text: text.into(),
        }
    }

    #[test]
    fn a_turn_takes_strong_matches_personal_details_first() {
        let hits = vec![
            recollection(Corpus::Message, "m", 0.91, "We booked the Mombasa train."),
            recollection(Corpus::Digest, "d", 0.70, "Below the floor."),
            recollection(Corpus::Personal, "p", 0.80, "Interested in trains (keen)."),
            recollection(Corpus::Report, "r", 0.85, "Train fares compared."),
        ];
        let kept = for_turn(hits, 0.77, 2);
        let ids: Vec<&str> = kept.iter().map(|h| h.row_id.as_str()).collect();
        assert_eq!(ids, vec!["p", "m"]);
    }

    #[test]
    fn the_block_keeps_the_first_hit_cut_to_the_budget_and_drops_what_does_not_fit() {
        let long = "word ".repeat(200);
        let hits = vec![
            recollection(Corpus::Message, "m", 0.9, &long),
            recollection(Corpus::Idea, "i", 0.8, "A second, short hit."),
        ];
        let (block, held) = render_turn_block(&hits, 20);
        assert_eq!(held, 1);
        assert!(block.starts_with("<personal-context>\n- (said in an earlier conversation) word"));
        assert!(block.ends_with("…\n</personal-context>"), "{block}");
        assert!(!block.contains("second"), "{block}");
        assert!(block.chars().count() <= 20 * 4 + 45, "{block}");

        let (roomy, held) = render_turn_block(&hits, 1_000);
        assert_eq!(held, 2);
        assert!(roomy.contains("- (from an idea I suggested) A second, short hit."));
        assert_eq!(render_turn_block(&[], 1_000), (String::new(), 0));
    }

    #[test]
    fn a_personal_detail_says_which_kind_it_is() {
        assert_eq!(label(Corpus::Personal, "saved:abc"), "something you saved");
        assert_eq!(
            label(Corpus::Personal, "interest:abc"),
            "from your interests"
        );
        assert_eq!(label(Corpus::Personal, "profile:abc"), "from your profile");
        assert_eq!(
            label(Corpus::Digest, "saved:abc"),
            provenance(Corpus::Digest)
        );
    }

    #[test]
    fn a_line_is_one_line() {
        let line = turn_line(
            &recollection(Corpus::Notice, "n", 0.9, "Rain\n  at   noon"),
            100,
        );
        assert_eq!(line, "- (from a notification I sent) Rain at noon");

        let cut = turn_line(
            &recollection(Corpus::Story, "s", 0.9, "Ruto and Sifuna tied at 31%"),
            21,
        );
        assert_eq!(
            cut,
            "- (from a news story I picked for you) Ruto and Sifuna tied…"
        );
    }

    struct StubIndex(Vec<ResolvedHit>);

    #[async_trait]
    impl VectorIndex for StubIndex {
        async fn upsert(&self, _e: &VectorEntry) -> Result<()> {
            Ok(())
        }
        async fn remove(&self, _c: Corpus, _r: &str) -> Result<()> {
            Ok(())
        }
        async fn get(&self, _c: Corpus, _r: &str) -> Result<Option<VectorEntry>> {
            Ok(None)
        }
        async fn search(
            &self,
            _q: &[f32],
            _m: &str,
            _s: &ProfileScope,
            _l: usize,
        ) -> Result<Vec<VectorHit>> {
            Ok(vec![])
        }
        async fn search_resolved_excluding(
            &self,
            _q: &[f32],
            _m: &str,
            scope: &ProfileScope,
            _l: usize,
            _x: &Exclusions,
        ) -> Result<Vec<ResolvedHit>> {
            // Mirrors the real index refusing a guest in SQL.
            if scope.excludes_everything() {
                return Ok(vec![]);
            }
            Ok(self.0.clone())
        }
        async fn needs_embedding(&self, _c: Corpus, _m: &str, _l: usize) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn needs_embedding_with_text(
            &self,
            _c: Corpus,
            _m: &str,
            _l: usize,
        ) -> Result<Vec<StaleRow>> {
            Ok(vec![])
        }
        async fn prune_orphans(&self) -> Result<u64> {
            Ok(0)
        }
        async fn health(&self, _m: &str) -> Result<IndexHealth> {
            // Empty, but one row per corpus: the port requires every corpus, so a dead one shows.
            Ok(IndexHealth {
                per_corpus: Corpus::ALL
                    .into_iter()
                    .map(|corpus| CorpusHealth {
                        corpus,
                        rows: 0,
                        source_rows: 0,
                        indexed_rows: 0,
                        missing_rows: 0,
                        mismatched: 0,
                    })
                    .collect(),
                ..IndexHealth::default()
            })
        }
    }

    struct StubEmbedder;

    #[async_trait]
    impl EmbeddingProvider for StubEmbedder {
        async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            Ok(vec![1.0, 0.0])
        }
        fn dimensions(&self) -> usize {
            2
        }
        fn model_id(&self) -> String {
            "stub".into()
        }
    }

    fn hit(corpus: Corpus, id: &str, score: f32, text: &str) -> ResolvedHit {
        ResolvedHit {
            corpus,
            row_id: id.into(),
            score,
            text: text.into(),
        }
    }

    fn service(hits: Vec<ResolvedHit>) -> PersonalContextRetrieval {
        PersonalContextRetrieval::new(Arc::new(StubIndex(hits)), Arc::new(StubEmbedder))
    }

    #[tokio::test]
    async fn a_guest_recalls_nothing() {
        let svc = service(vec![hit(Corpus::Memory, "m1", 0.9, "secret")]);
        assert!(svc
            .recall("anything", &ProfileScope::Guest, 5)
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn an_empty_query_asks_the_index_nothing() {
        let svc = service(vec![hit(Corpus::Memory, "m1", 0.9, "x")]);
        assert!(svc
            .recall("   ", &ProfileScope::Household, 5)
            .await
            .is_empty());
        assert!(svc
            .recall("x", &ProfileScope::Household, 0)
            .await
            .is_empty());
    }

    /// The index row and live text are separate reads; a summary can be blanked in between.
    #[tokio::test]
    async fn a_hit_with_no_live_text_is_dropped() {
        let svc = service(vec![
            hit(Corpus::Memory, "m1", 0.9, "   "),
            hit(Corpus::Memory, "m2", 0.8, "real"),
        ]);
        let got = svc.recall("q", &ProfileScope::Household, 5).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].row_id, "m2");
    }

    #[tokio::test]
    async fn results_carry_their_corpus_and_can_introduce_themselves() {
        let svc = service(vec![
            hit(Corpus::Memory, "m1", 0.9, "the bill is eighty pounds"),
            hit(Corpus::Summary, "s1", 0.8, "we discussed the garden"),
        ]);
        let got = svc.recall("q", &ProfileScope::Household, 5).await;
        assert_eq!(got[0].corpus, Corpus::Memory);
        assert_eq!(got[1].corpus, Corpus::Summary);
        assert!(got[0].labelled().starts_with("(you told me)"));
        assert!(got[1]
            .labelled()
            .starts_with("(from an earlier conversation)"));
    }
}
