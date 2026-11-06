//! One retrieval over every indexed kind, scoped in [`VectorIndex`]'s SQL.
//! Each hit carries its kind: a summary is lossy, not a said fact.

use std::sync::Arc;

use anyhow::Result;

use crate::context::vector_index::{ResolvedHit, VectorIndex, INDEXED};
use crate::items::Kind;
use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::domain::profile::ProfileScope;

/// How a hit is introduced to the model and member: prose, not the kind's name.
pub fn provenance(kind: Kind) -> &'static str {
    match kind {
        Kind::Fact | Kind::Trait | Kind::Entity => "you told me",
        Kind::Conversation | Kind::Message => "from an earlier conversation",
        Kind::Mail
        | Kind::Event
        | Kind::Notification
        | Kind::HomeEvent
        | Kind::Media
        | Kind::Visit => "observed by this pond",
        Kind::Report => "from a finished task",
        Kind::Suggestion => "suggested by this pond",
    }
}

/// A retrieval answer: the text, where it came from, and how well it matched.
#[derive(Debug, Clone, PartialEq)]
pub struct Recollection {
    pub kind: Kind,
    pub item_id: String,
    pub score: f32,
    pub text: String,
}

impl Recollection {
    /// The hit, introduced by its provenance.
    pub fn labelled(&self) -> String {
        format!("({}) {}", provenance(self.kind), self.text)
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
            .search_resolved(&vector, &model_id, scope, &INDEXED, limit)
            .await?;

        Ok(hits
            .into_iter()
            .filter(|h| !h.text.trim().is_empty())
            .map(|h| Recollection {
                kind: h.kind,
                item_id: h.item_id,
                score: h.score,
                text: h.text,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::vector_index::{IndexHealth, Stale, VectorEntry, VectorHit};
    use async_trait::async_trait;

    #[test]
    fn every_kind_states_its_provenance_in_words() {
        for k in Kind::ALL {
            let p = provenance(k);
            assert!(!p.is_empty());
            assert_ne!(p, k.as_str());
        }
        let r = Recollection {
            kind: Kind::Conversation,
            item_id: "s1".into(),
            score: 0.9,
            text: "we agreed on Tuesday".into(),
        };
        assert_eq!(
            r.labelled(),
            "(from an earlier conversation) we agreed on Tuesday"
        );
    }

    struct StubIndex(Vec<ResolvedHit>);

    #[async_trait]
    impl VectorIndex for StubIndex {
        async fn upsert(&self, _e: &VectorEntry) -> Result<()> {
            Ok(())
        }
        async fn remove(&self, _i: &str) -> Result<()> {
            Ok(())
        }
        async fn get(&self, _i: &str) -> Result<Option<VectorEntry>> {
            Ok(None)
        }
        async fn search(
            &self,
            _q: &[f32],
            _m: &str,
            _s: &ProfileScope,
            _k: &[Kind],
            _l: usize,
        ) -> Result<Vec<VectorHit>> {
            Ok(vec![])
        }
        async fn search_resolved(
            &self,
            _q: &[f32],
            _m: &str,
            scope: &ProfileScope,
            _k: &[Kind],
            _l: usize,
        ) -> Result<Vec<ResolvedHit>> {
            // Mirrors the real index refusing a guest in SQL.
            if scope.excludes_everything() {
                return Ok(vec![]);
            }
            Ok(self.0.clone())
        }
        async fn needs_embedding(&self, _k: &[Kind], _m: &str, _l: usize) -> Result<Vec<Stale>> {
            Ok(vec![])
        }
        async fn prune_orphans(&self) -> Result<u64> {
            Ok(0)
        }
        async fn health(&self, _m: &str) -> Result<IndexHealth> {
            Ok(IndexHealth::default())
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

    fn hit(kind: Kind, id: &str, score: f32, text: &str) -> ResolvedHit {
        ResolvedHit {
            kind,
            item_id: id.into(),
            score,
            text: text.into(),
        }
    }

    fn service(hits: Vec<ResolvedHit>) -> PersonalContextRetrieval {
        PersonalContextRetrieval::new(Arc::new(StubIndex(hits)), Arc::new(StubEmbedder))
    }

    #[tokio::test]
    async fn a_guest_recalls_nothing() {
        let svc = service(vec![hit(Kind::Fact, "m1", 0.9, "secret")]);
        assert!(svc
            .recall("anything", &ProfileScope::Guest, 5)
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn an_empty_query_asks_the_index_nothing() {
        let svc = service(vec![hit(Kind::Fact, "m1", 0.9, "x")]);
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
            hit(Kind::Fact, "m1", 0.9, "   "),
            hit(Kind::Fact, "m2", 0.8, "real"),
        ]);
        let got = svc.recall("q", &ProfileScope::Household, 5).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].item_id, "m2");
    }

    #[tokio::test]
    async fn results_carry_their_kind_and_can_introduce_themselves() {
        let svc = service(vec![
            hit(Kind::Fact, "m1", 0.9, "the bill is eighty pounds"),
            hit(Kind::Conversation, "s1", 0.8, "we discussed the garden"),
        ]);
        let got = svc.recall("q", &ProfileScope::Household, 5).await;
        assert_eq!(got[0].kind, Kind::Fact);
        assert_eq!(got[1].kind, Kind::Conversation);
        assert!(got[0].labelled().starts_with("(you told me)"));
        assert!(got[1]
            .labelled()
            .starts_with("(from an earlier conversation)"));
    }
}
