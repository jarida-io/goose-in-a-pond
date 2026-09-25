use anyhow::Result;
use async_trait::async_trait;

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

    /// The dimension of vectors returned by this provider (e.g. 384 for MiniLM).
    fn dimensions(&self) -> usize;

    /// Stable embedder id stamped on stored rows: a foreign vector scores plausibly but wrongly.
    /// Override the placeholder wherever vectors outlive the process.
    fn model_id(&self) -> String {
        "unspecified".to_string()
    }
}
