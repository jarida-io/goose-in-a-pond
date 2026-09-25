//! Fastembed (ONNX) `EmbeddingProvider`; downloads its model from HuggingFace on first use.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use pond_core::models::ports::embedding::EmbeddingProvider;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const MINILM_L6_V2_DIMS: usize = 384;
const BGE_SMALL_EN_DIMS: usize = 384;

fn resolve_model(name: &str) -> Result<(EmbeddingModel, usize)> {
    match name {
        "all-MiniLM-L6-v2" | "" => Ok((EmbeddingModel::AllMiniLML6V2, MINILM_L6_V2_DIMS)),
        "bge-small-en-v1.5" => Ok((EmbeddingModel::BGESmallENV15, BGE_SMALL_EN_DIMS)),
        other => Err(anyhow!(
            "Unknown embedding model: '{}'. Supported: all-MiniLM-L6-v2, bge-small-en-v1.5",
            other
        )),
    }
}

/// Fastembed-backed provider. Construction blocks (loads, maybe downloads, the ONNX model).
pub struct FastembedEmbeddingProvider {
    model: Arc<Mutex<TextEmbedding>>,
    dims: usize,
    model_name: String,
}

impl FastembedEmbeddingProvider {
    /// Provider for `model_name`; `cache_dir: None` uses fastembed's default cache location.
    pub fn new(model_name: &str, cache_dir: Option<PathBuf>) -> Result<Self> {
        let (variant, dims) = resolve_model(model_name)?;

        let mut opts = InitOptions::new(variant).with_show_download_progress(true);

        if let Some(dir) = cache_dir {
            std::fs::create_dir_all(&dir)?;
            opts = opts.with_cache_dir(dir);
        }

        let embedding = TextEmbedding::try_new(opts)?;

        let effective_name = if model_name.is_empty() {
            "all-MiniLM-L6-v2"
        } else {
            model_name
        };

        tracing::info!(
            model = effective_name,
            dims = dims,
            "fastembed embedding model loaded"
        );

        Ok(Self {
            model: Arc::new(Mutex::new(embedding)),
            dims,
            model_name: effective_name.to_string(),
        })
    }

    /// Human-readable model name (for logging / status endpoints).
    pub fn model_name(&self) -> &str {
        &self.model_name
    }
}

#[async_trait]
impl EmbeddingProvider for FastembedEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let model = Arc::clone(&self.model);
        let owned = text.to_string();

        let result = tokio::task::spawn_blocking(move || {
            let mut guard = model
                .lock()
                .map_err(|e| anyhow!("fastembed mutex poisoned: {e}"))?;
            let embeddings = guard
                .embed(vec![owned], None)
                .map_err(|e| anyhow!("fastembed embed failed: {e}"))?;
            embeddings
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("fastembed returned empty embeddings"))
        })
        .await
        .map_err(|e| anyhow!("spawn_blocking join error: {e}"))??;

        Ok(result)
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn model_id(&self) -> String {
        self.model_name.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_known_models() {
        let (model, dims) = resolve_model("all-MiniLM-L6-v2").unwrap();
        assert_eq!(dims, 384);
        assert!(matches!(model, EmbeddingModel::AllMiniLML6V2));

        let (model, dims) = resolve_model("bge-small-en-v1.5").unwrap();
        assert_eq!(dims, 384);
        assert!(matches!(model, EmbeddingModel::BGESmallENV15));
    }

    #[test]
    fn resolve_empty_defaults_to_minilm() {
        let (model, dims) = resolve_model("").unwrap();
        assert_eq!(dims, 384);
        assert!(matches!(model, EmbeddingModel::AllMiniLML6V2));
    }

    #[test]
    fn resolve_unknown_model_errors() {
        let err = resolve_model("nonexistent-model").unwrap_err();
        assert!(err.to_string().contains("Unknown embedding model"));
    }
}
