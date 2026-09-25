//! The ingest pipeline: raw item, redact, classify, persist, embed.
//! Redaction lives in [`ContextItem::from_parts`], the only constructor, so nothing bypasses it.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::context::domain::{
    ContextError, ContextItem, ContextSource, ItemKind, ItemParts, SourceAvailability, SourceKind,
};
use crate::context::ports::ContextRepository;
use crate::models::ports::embedding::EmbeddingProvider;
use crate::security::domain::event::PrivacySensitivity;
use crate::security::domain::redaction::RedactionKind;
use crate::security::ports::redactor::Redactor;

/// One thing a source produced, before this pond has touched it.
#[derive(Debug, Clone, PartialEq)]
pub struct RawItem {
    /// The upstream's own id. What makes a re-sync idempotent.
    pub external_id: String,
    pub kind: ItemKind,
    pub occurred_at: DateTime<Utc>,
    pub title: String,
    pub body: String,
    pub participants: Vec<String>,
}

/// What one ingested item ended up as.
#[derive(Debug, Clone, PartialEq)]
pub struct IngestOutcome {
    pub item_id: String,
    pub sensitivity: PrivacySensitivity,
    /// Classes of personal data the redactor found. Never the matched text.
    pub findings: Vec<RedactionKind>,
}

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("cannot ingest from a {kind} source: {reason}")]
    SourceUnavailable {
        kind: &'static str,
        reason: &'static str,
    },
    #[error(transparent)]
    Invalid(#[from] ContextError),
    #[error("could not store the context item: {0}")]
    Storage(#[from] anyhow::Error),
}

pub struct IngestPipeline {
    repo: Arc<dyn ContextRepository>,
    redactor: Arc<dyn Redactor>,
    embedder: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
}

impl IngestPipeline {
    /// Not an `Option`: a "no redactor wired" fallback would silently skip redaction.
    pub fn new(repo: Arc<dyn ContextRepository>, redactor: Arc<dyn Redactor>) -> Self {
        Self {
            repo,
            redactor,
            embedder: None,
        }
    }

    /// Attach an embedding provider; without one items stay keyword-searchable.
    pub fn with_embedder(
        mut self,
        embedder: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    ) -> Self {
        self.embedder = embedder;
        self
    }

    /// Deterministic row id, so a re-ingest hits the same primary key, not just the UNIQUE pair.
    pub fn item_id(source_id: &str, external_id: &str) -> String {
        format!("{source_id}:{external_id}")
    }

    /// Ingest one item. The owner comes from `source`, never from the payload.
    pub async fn ingest(
        &self,
        source: &ContextSource,
        raw: RawItem,
        now: DateTime<Utc>,
    ) -> Result<IngestOutcome, IngestError> {
        let availability = source.kind().availability();
        if availability != SourceAvailability::Landed {
            return Err(IngestError::SourceUnavailable {
                kind: source.kind().as_str(),
                reason: availability.refusal(),
            });
        }

        let item = ContextItem::from_parts(
            self.redactor.as_ref(),
            ItemParts {
                id: Self::item_id(source.id(), &raw.external_id),
                source_id: source.id().to_string(),
                external_id: raw.external_id,
                profile_id: source.profile_id().to_string(),
                source_kind: source.kind(),
                kind: raw.kind,
                occurred_at: raw.occurred_at,
                ingested_at: now,
                title: raw.title,
                body: raw.body,
                participants: raw.participants,
                stored_sensitivity: None,
                embedding: None,
            },
        )?;

        if !item.findings().is_empty() {
            let kinds: Vec<&str> = item.findings().iter().map(|k| k.as_str()).collect();
            tracing::info!(
                target: "giap::trace",
                item_id = %item.id(),
                source_id = %item.source_id(),
                kinds = %kinds.join(","),
                "[redaction] removed credential-shaped material before storing a context item"
            );
        }

        // Only one-chunk text is embedded inline; a long body is left unembedded for the sweep to
        // chunk, since an item with a vector looks indexed and would never be picked up.
        let text = item.embedding_text();
        let one_chunk = text.len() <= crate::context::chunking::DEFAULT_CHUNK_BYTES;
        let item = match (&self.embedder, one_chunk) {
            (Some(embedder), true) => match embedder.embed(&text).await {
                Ok(vector) => item.with_embedding(vector),
                Err(e) => {
                    // Not fatal: `search_unembedded` + `update_embedding` backfill it.
                    tracing::warn!(error = %e, item_id = %item.id(), "context item stored unembedded");
                    item
                }
            },
            // Long enough to chunk: stored now, passages embedded by the sweep.
            (Some(_), false) => item,
            (None, _) => item,
        };

        self.repo.save_item(&item).await?;

        Ok(IngestOutcome {
            item_id: item.id().to_string(),
            sensitivity: item.sensitivity(),
            findings: item.findings().to_vec(),
        })
    }

    /// Ingest a batch, per-item outcomes in order; one bad item must not stick the cursor on it.
    pub async fn ingest_all(
        &self,
        source: &ContextSource,
        raws: Vec<RawItem>,
        now: DateTime<Utc>,
    ) -> Vec<Result<IngestOutcome, IngestError>> {
        let mut out = Vec::with_capacity(raws.len());
        for raw in raws {
            out.push(self.ingest(source, raw, now).await);
        }
        out
    }
}

/// Every source kind this pipeline will accept today.
pub fn ingestable_kinds() -> Vec<SourceKind> {
    SourceKind::ALL
        .into_iter()
        .filter(|k| k.availability() == SourceAvailability::Landed)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::domain::{SourceParts, SourceStatus};
    use crate::context::mocks::mock_context_repository::MockContextRepository;
    use crate::security::mocks::mock_redactor::MockRedactor;
    use crate::user_data::domain::profile::ProfileScope;

    const KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz123456";

    /// One source per kind, with a per-kind id so ingests don't overwrite each other.
    fn source(kind: SourceKind) -> ContextSource {
        ContextSource::from_parts(SourceParts {
            id: format!("src-{}", kind.as_str()),
            kind,
            provider: "pond".into(),
            profile_id: "jerry".into(),
            scopes: vec![],
            cursor: None,
            last_sync: None,
            status: SourceStatus::Connected,
            secret_ref: None,
            created_at: Utc::now(),
        })
        .expect("valid source")
    }

    fn raw(external_id: &str, body: &str) -> RawItem {
        RawItem {
            external_id: external_id.into(),
            kind: ItemKind::Message,
            occurred_at: Utc::now(),
            title: "a subject".into(),
            body: body.into(),
            participants: vec![],
        }
    }

    fn wire() -> (Arc<MockContextRepository>, IngestPipeline) {
        let repo = Arc::new(MockContextRepository::new());
        let redactor = Arc::new(MockRedactor::replacing(KEY, RedactionKind::ApiKey));
        let pipeline = IngestPipeline::new(repo.clone(), redactor);
        (repo, pipeline)
    }

    #[tokio::test]
    async fn the_stored_row_is_the_redacted_one() {
        let (repo, pipeline) = wire();
        pipeline
            .ingest(
                &source(SourceKind::Voice),
                raw("e1", &format!("he said {KEY} out loud")),
                Utc::now(),
            )
            .await
            .expect("ingest");

        let stored = repo
            .recent_items(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert!(
            !stored[0].body().contains(KEY),
            "the raw body reached storage: {}",
            stored[0].body()
        );
        assert!(stored[0].body().contains("[redacted:api-key]"));
    }

    /// An embedder that records the text it was handed.
    struct RecordingEmbedder {
        seen: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl EmbeddingProvider for RecordingEmbedder {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            self.seen.lock().unwrap().push(text.to_string());
            Ok(vec![0.25; 8])
        }
        fn dimensions(&self) -> usize {
            8
        }
    }

    /// Pins the order: the stored-body check stays green if the embed moves above redaction.
    #[tokio::test]
    async fn the_vector_is_computed_from_the_redacted_text() {
        let repo = Arc::new(MockContextRepository::new());
        let redactor = Arc::new(MockRedactor::replacing(KEY, RedactionKind::ApiKey));
        let embedder = Arc::new(RecordingEmbedder {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let pipeline = IngestPipeline::new(repo.clone(), redactor)
            .with_embedder(Some(embedder.clone() as Arc<dyn EmbeddingProvider>));

        pipeline
            .ingest(
                &source(SourceKind::Voice),
                raw("e1", &format!("he said {KEY} out loud")),
                Utc::now(),
            )
            .await
            .expect("ingest");

        let seen = embedder.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "expected exactly one embed call");
        assert!(
            !seen[0].contains(KEY),
            "the embedder was handed the UNREDACTED text, so the stored vector \
             is a derivative of a secret: {}",
            seen[0]
        );
        assert!(
            seen[0].contains("[redacted:api-key]"),
            "expected the redacted form to be what was embedded: {}",
            seen[0]
        );

        // And the vector actually reached the row.
        let stored = repo
            .recent_items(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].embedding().map(|e| e.len()),
            Some(8),
            "the item was stored without the vector the pipeline computed"
        );
    }

    #[tokio::test]
    async fn a_connector_source_is_refused_until_its_gate_lands() {
        let (repo, pipeline) = wire();
        let mut refused = 0usize;
        for kind in SourceKind::ALL {
            let result = pipeline
                .ingest(&source(kind), raw("e1", "hello"), Utc::now())
                .await;
            match kind.availability() {
                SourceAvailability::Landed => {
                    assert!(
                        result.is_ok(),
                        "{} should ingest: {result:?}",
                        kind.as_str()
                    )
                }
                _ => {
                    refused += 1;
                    let message = result.expect_err("must refuse").to_string();
                    assert!(
                        message.contains(kind.as_str()),
                        "the refusal does not say which kind: {message}"
                    );
                    assert!(
                        message.contains("ingest route") || message.contains("connector"),
                        "the refusal does not name what has to land first: {message}"
                    );
                }
            }
        }
        assert_eq!(
            refused,
            SourceKind::ALL.len() - ingestable_kinds().len(),
            "the sweep did not refuse every unlanded kind"
        );
        assert_eq!(
            repo.recent_items(&ProfileScope::Household, 100)
                .await
                .unwrap()
                .len(),
            ingestable_kinds().len(),
            "a refused ingest still wrote a row"
        );
    }

    /// Vacuity control for the refusal sweep above.
    #[tokio::test]
    async fn the_refusal_sweep_has_something_to_refuse_and_something_to_accept() {
        assert!(
            !ingestable_kinds().is_empty(),
            "nothing can be ingested at all"
        );
        assert!(
            ingestable_kinds().len() < SourceKind::ALL.len(),
            "every kind is landed, so the refusal branch above is never taken"
        );
    }

    /// Checks the type too: a field added to `RawItem` is how this would stop holding.
    #[tokio::test]
    async fn the_owner_comes_from_the_source_and_the_payload_cannot_say() {
        let (repo, pipeline) = wire();
        pipeline
            .ingest(&source(SourceKind::Sensor), raw("e1", "hot"), Utc::now())
            .await
            .expect("ingest");
        let stored = repo
            .recent_items(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(stored[0].profile_id(), "jerry");

        const SRC: &str = include_str!("ingest.rs");
        let decl = SRC
            .split("pub struct RawItem {")
            .nth(1)
            .and_then(|s| s.split("\n}").next())
            .expect("RawItem is no longer declared here");
        assert!(
            !decl.contains("profile_id"),
            "RawItem grew a profile_id. The owner must come from the source; a payload that \
             names its own owner lets whatever pushed it decide whose data this is."
        );
    }

    #[tokio::test]
    async fn re_ingesting_the_same_upstream_item_updates_rather_than_duplicates() {
        let (repo, pipeline) = wire();
        let src = source(SourceKind::Voice);
        let first = pipeline
            .ingest(&src, raw("e1", "first version"), Utc::now())
            .await
            .expect("ingest");
        let second = pipeline
            .ingest(&src, raw("e1", "corrected version"), Utc::now())
            .await
            .expect("ingest");
        assert_eq!(
            first.item_id, second.item_id,
            "the id must be derived, not random"
        );

        let stored = repo
            .recent_items(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1, "a re-sync duplicated the row");
        assert_eq!(stored[0].body(), "corrected version");
    }

    #[tokio::test]
    async fn a_malformed_item_does_not_abandon_the_batch() {
        let (repo, pipeline) = wire();
        let mut bad = raw("", "no external id");
        bad.title = "x".into();
        let results = pipeline
            .ingest_all(
                &source(SourceKind::Voice),
                vec![raw("e1", "one"), bad, raw("e2", "three")],
                Utc::now(),
            )
            .await;
        assert_eq!(results.len(), 3);
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
        assert!(results[2].is_ok());
        assert_eq!(
            repo.recent_items(&ProfileScope::Household, 10)
                .await
                .unwrap()
                .len(),
            2
        );
    }
}
