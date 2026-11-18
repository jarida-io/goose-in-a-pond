//! Fastembed (ONNX) `EmbeddingProvider`; downloads its model from HuggingFace on first use.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use pond_core::models::ports::embedding::EmbeddingProvider;
use std::path::{Path, PathBuf};
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

/// Where fastembed keeps its weights when the pond hands it `dir`: `HF_HOME` wins, as it does
/// inside fastembed's own download (`fastembed::common::pull_from_hf`).
pub fn cache_root(dir: &Path) -> PathBuf {
    std::env::var_os("HF_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.to_path_buf())
}

/// The Hugging Face cache entry `model_name`'s weights land in under `root`
/// (`models--<org>--<repo>`), or `None` for a model fastembed does not serve.
pub fn cached_repo_dir(model_name: &str, root: &Path) -> Option<PathBuf> {
    let (variant, _) = resolve_model(model_name).ok()?;
    let info = TextEmbedding::get_model_info(&variant).ok()?;
    Some(root.join(format!("models--{}", info.model_code.replace('/', "--"))))
}

/// Whether `model_name`'s weights are already cached under `root`, so loading it needs no
/// download: the snapshot `refs/main` names holds both the ONNX file and the tokenizer.
pub fn weights_cached(model_name: &str, root: &Path) -> bool {
    let Ok((variant, _)) = resolve_model(model_name) else {
        return false;
    };
    let (Some(repo), Ok(info)) = (
        cached_repo_dir(model_name, root),
        TextEmbedding::get_model_info(&variant),
    ) else {
        return false;
    };
    let Ok(revision) = std::fs::read_to_string(repo.join("refs").join("main")) else {
        return false;
    };
    let snapshot = repo.join("snapshots").join(revision.trim());
    // `is_file` follows the snapshot's links into `blobs/`, so a link left dangling reads absent.
    snapshot.join(&info.model_file).is_file() && snapshot.join("tokenizer.json").is_file()
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

/// fastembed's own default batch size; the embedding slot's time budget usually packs fewer.
const MAX_BATCH: usize = 256;

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

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let model = Arc::clone(&self.model);
        let owned = texts.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut guard = model
                .lock()
                .map_err(|e| anyhow!("fastembed mutex poisoned: {e}"))?;
            guard
                .embed(owned, None)
                .map_err(|e| anyhow!("fastembed embed failed: {e}"))
        })
        .await
        .map_err(|e| anyhow!("spawn_blocking join error: {e}"))?
    }

    fn max_batch(&self) -> usize {
        MAX_BATCH
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

    #[cfg(unix)]
    /// Lay out a model's cache as hf-hub leaves it, the snapshot's files linking into `blobs/`.
    fn cache(root: &Path, repo: &str, model_file: &str) {
        let repo = root.join(repo);
        let snapshot = repo.join("snapshots").join("abc123");
        std::fs::create_dir_all(snapshot.join(model_file).parent().unwrap()).unwrap();
        std::fs::create_dir_all(repo.join("refs")).unwrap();
        std::fs::create_dir_all(repo.join("blobs")).unwrap();
        std::fs::write(repo.join("refs").join("main"), "abc123").unwrap();
        for (name, blob) in [(model_file, "b1"), ("tokenizer.json", "b2")] {
            std::fs::write(repo.join("blobs").join(blob), b"x").unwrap();
            std::os::unix::fs::symlink(repo.join("blobs").join(blob), snapshot.join(name)).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn cached_weights_are_found_for_each_built_in_model() {
        let root = tempfile::tempdir().unwrap();
        assert!(!weights_cached("all-MiniLM-L6-v2", root.path()));
        assert!(!weights_cached("bge-small-en-v1.5", root.path()));

        cache(
            root.path(),
            "models--Qdrant--all-MiniLM-L6-v2-onnx",
            "model.onnx",
        );
        cache(
            root.path(),
            "models--Xenova--bge-small-en-v1.5",
            "onnx/model.onnx",
        );
        assert!(weights_cached("all-MiniLM-L6-v2", root.path()));
        assert!(weights_cached("bge-small-en-v1.5", root.path()));
    }

    #[cfg(unix)]
    #[test]
    fn weights_whose_blob_is_gone_are_not_cached() {
        let root = tempfile::tempdir().unwrap();
        cache(
            root.path(),
            "models--Qdrant--all-MiniLM-L6-v2-onnx",
            "model.onnx",
        );
        let blobs = root
            .path()
            .join("models--Qdrant--all-MiniLM-L6-v2-onnx")
            .join("blobs");
        std::fs::remove_file(blobs.join("b1")).unwrap();
        assert!(!weights_cached("all-MiniLM-L6-v2", root.path()));
    }

    #[test]
    fn a_model_fastembed_does_not_serve_has_no_cache() {
        let root = tempfile::tempdir().unwrap();
        assert!(!weights_cached("embeddinggemma-2", root.path()));
        assert!(cached_repo_dir("embeddinggemma-2", root.path()).is_none());
    }

    #[test]
    fn resolve_unknown_model_errors() {
        let err = resolve_model("nonexistent-model").unwrap_err();
        assert!(err.to_string().contains("Unknown embedding model"));
    }
}
