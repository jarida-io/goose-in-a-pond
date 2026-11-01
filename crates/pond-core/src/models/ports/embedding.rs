use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::models::domain::embedding_models::EmbeddingModel;

/// Text embedding; all vectors from one provider must share one dimensionality.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed a stored DOCUMENT: anything written to the corpus (memory, context item, summary).
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed a search QUERY. Asymmetric models want a different task prefix (nomic-embed-text:
    /// `search_query: `); the vector must be in the SAME space and width as [`Self::embed`].
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text).await
    }

    /// Embed several stored documents, one vector per text in order; callers pass at most
    /// [`Self::max_batch`] texts.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut vectors = Vec::with_capacity(texts.len());
        for text in texts {
            vectors.push(self.embed(text).await?);
        }
        Ok(vectors)
    }

    /// The most texts one [`Self::embed_batch`] call takes.
    fn max_batch(&self) -> usize {
        1
    }

    /// The dimension of vectors returned by this provider (e.g. 384 for MiniLM).
    fn dimensions(&self) -> usize;

    /// Stable embedder id stamped on stored rows: a foreign vector scores plausibly but wrongly.
    /// Override the placeholder wherever vectors outlive the process.
    fn model_id(&self) -> String {
        "unspecified".to_string()
    }

    /// Whether a model is loaded and embedding now; semantic search is on only while it is.
    fn available(&self) -> bool {
        true
    }

    /// Changes whenever the model behind this provider changes, so a cache of its vectors can tell.
    fn generation(&self) -> u64 {
        0
    }
}

/// Builds the provider for one embedding model; the only place a model is loaded.
#[async_trait]
pub trait EmbedderFactory: Send + Sync {
    async fn build(&self, model: &'static EmbeddingModel) -> Result<Arc<dyn EmbeddingProvider>>;
}
