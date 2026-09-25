//! GGUF-backed [`EmbeddingProvider`] over llama.cpp, used where fastembed's ONNX Runtime fails
//! (Jetson Orin). Coexists with Goose's llama.cpp via [`get_or_init_backend`], not load order.
//!
//! [`get_or_init_backend`]: crate::engine::get_or_init_backend

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use pond_core::models::ports::embedding::EmbeddingProvider;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Per-text token cap for pathological inputs; `min`'d with the model's trained context.
const MAX_EMBED_TOKENS: usize = 2048;

/// Everything that makes one embedding model's vectors incompatible with another's.
#[derive(Clone, Debug)]
pub struct EmbeddingModelSpec {
    /// Stable identifier stamped onto every vector this model produces.
    pub model_id: String,
    /// GGUF filename expected under `{data_dir}/models/embedding/`.
    pub filename: String,
    /// Output dimensionality. Asserted against the loaded model's `n_embd`.
    pub dims: usize,
    /// Sequence pooling (nomic mean, bge CLS); the wrong one silently yields worse vectors.
    pub pooling: LlamaPoolingType,
    /// Task prefix prepended to a stored DOCUMENT (nomic: `search_document: `, bge: none).
    pub content_prefix: &'static str,
    /// Task prefix for a QUERY, used by `embed_query` (nomic: `search_query: `, bge: none).
    pub query_prefix: &'static str,
    /// GPU layers to offload; 0 keeps this tiny pass off the GPU the chat model needs.
    pub n_gpu_layers: u32,
    /// ggml threads; small because llama.cpp's default 4 starves chat prefill on a 6-core Orin.
    pub n_threads: i32,
    /// Download URL for `filename`; fetch it only via a gated (egress-tracked) downloader.
    pub download_url: String,
    /// Approximate download size, for progress display only.
    pub size_hint_mb: u64,
}

impl EmbeddingModelSpec {
    /// Default: `nomic-embed-text-v1.5`, 768-dim, mean-pooled, retrieval-tuned.
    pub fn nomic_embed_text_v1_5() -> Self {
        Self {
            model_id: "nomic-embed-text-v1.5".to_string(),
            filename: "nomic-embed-text-v1.5.Q8_0.gguf".to_string(),
            dims: 768,
            pooling: LlamaPoolingType::Mean,
            // nomic needs a task prefix, and a different one per side.
            content_prefix: "search_document: ",
            query_prefix: "search_query: ",
            n_gpu_layers: 0,
            n_threads: 2,
            download_url: "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5-GGUF/\
                           resolve/main/nomic-embed-text-v1.5.Q8_0.gguf"
                .to_string(),
            size_hint_mb: 146,
        }
    }

    /// `bge-base-en-v1.5`, 768-dim, CLS-pooled: the fallback if nomic won't load. Never add a
    /// 384-dim model: width is what tells these vectors apart from fastembed's 384-dim ones.
    pub fn bge_base_en_v1_5() -> Self {
        Self {
            model_id: "bge-base-en-v1.5".to_string(),
            filename: "bge-base-en-v1.5-q8_0.gguf".to_string(),
            dims: 768,
            pooling: LlamaPoolingType::Cls,
            content_prefix: "",
            query_prefix: "",
            n_gpu_layers: 0,
            n_threads: 2,
            download_url: "https://huggingface.co/CompendiumLabs/bge-base-en-v1.5-gguf/\
                           resolve/main/bge-base-en-v1.5-q8_0.gguf"
                .to_string(),
            size_hint_mb: 117,
        }
    }

    /// Resolve a settings model name ("" and "gguf" mean nomic). An unknown name is usually a
    /// leftover fastembed name (the setting is shared), so callers fall back to the default.
    pub fn resolve(name: &str) -> Result<Self> {
        match name {
            "" | "gguf" | "nomic-embed-text-v1.5" => Ok(Self::nomic_embed_text_v1_5()),
            "bge-base-en-v1.5" => Ok(Self::bge_base_en_v1_5()),
            other => Err(anyhow!(
                "unknown GGUF embedding model: '{other}'. \
                 Supported: nomic-embed-text-v1.5, bge-base-en-v1.5"
            )),
        }
    }

    /// Every GGUF embedding model this crate will load.
    #[cfg(test)]
    fn all() -> Vec<Self> {
        vec![Self::nomic_embed_text_v1_5(), Self::bge_base_en_v1_5()]
    }
}

/// Loaded model; contexts are made per call so nothing borrows the model self-referentially.
struct Loaded {
    backend: Arc<LlamaBackend>,
    model: LlamaModel,
    /// The per-call context is sized to at most this many tokens.
    max_ctx: usize,
}

/// [`EmbeddingProvider`] backed by a GGUF model via llama.cpp.
pub struct GgufEmbeddingProvider {
    model_path: PathBuf,
    spec: EmbeddingModelSpec,
    /// Loaded on first `embed`, keeping construction cheap and backend-free.
    loaded: Arc<Mutex<Option<Loaded>>>,
}

impl GgufEmbeddingProvider {
    /// Provider for `spec`'s GGUF in `embedding_dir`. Loads nothing; the file may not exist yet.
    pub fn new(spec: EmbeddingModelSpec, embedding_dir: &Path) -> Self {
        let model_path = embedding_dir.join(&spec.filename);
        Self {
            model_path,
            spec,
            loaded: Arc::new(Mutex::new(None)),
        }
    }

    /// The path the GGUF is expected at (for a startup existence check / fetch).
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    /// The vector-space stamp for stored vectors.
    pub fn model_id(&self) -> &str {
        &self.spec.model_id
    }
}

/// Load the model; blocking, so run it inside `spawn_blocking`.
fn load_sync(spec: &EmbeddingModelSpec, model_path: &Path) -> Result<Loaded> {
    if !model_path.exists() {
        return Err(anyhow!(
            "embedding model not found at {}. It must be downloaded (through a \
             gated downloader) before the provider can embed.",
            model_path.display()
        ));
    }

    // Safe even when Goose already initialised the backend.
    let backend = crate::engine::get_or_init_backend()
        .context("acquiring the shared llama backend for embeddings")?;

    let params = LlamaModelParams::default().with_n_gpu_layers(spec.n_gpu_layers);
    let model = LlamaModel::load_from_file(&backend, model_path, &params).map_err(|e| {
        anyhow!(
            "failed to load embedding model {}: {e}",
            model_path.display()
        )
    })?;

    // A width other than declared would poison every stored vector; refuse.
    let actual = usize::try_from(model.n_embd()).unwrap_or(0);
    if actual != spec.dims {
        return Err(anyhow!(
            "embedding model {} reports n_embd={actual} but the spec declares {} \
             dimensions — the vector space would be wrong. Refusing to load.",
            spec.model_id,
            spec.dims
        ));
    }

    let max_ctx = (model.n_ctx_train() as usize).min(MAX_EMBED_TOKENS).max(1);

    tracing::info!(
        model_id = %spec.model_id,
        dims = spec.dims,
        n_gpu_layers = spec.n_gpu_layers,
        max_ctx,
        "GGUF embedding model loaded"
    );

    Ok(Loaded {
        backend,
        model,
        max_ctx,
    })
}

/// Embed one text in a fresh context, returning the pooled, L2-normalised vector. Blocking.
fn embed_sync(
    loaded: &Loaded,
    spec: &EmbeddingModelSpec,
    text: &str,
    prefix: &str,
) -> Result<Vec<f32>> {
    let prefixed = format!("{prefix}{text}");

    let mut tokens = loaded
        .model
        .str_to_token(&prefixed, AddBos::Always)
        .map_err(|e| anyhow!("tokenising text for embedding failed: {e}"))?;
    if tokens.is_empty() {
        return Err(anyhow!("text produced no tokens to embed"));
    }
    if tokens.len() > loaded.max_ctx {
        // Truncated rows count as embedded and are never revisited, so warn.
        tracing::warn!(
            model_id = %spec.model_id,
            tokens = tokens.len(),
            kept = loaded.max_ctx,
            "embedding input truncated; the discarded tail is not searchable"
        );
        tokens.truncate(loaded.max_ctx);
    }
    let n = tokens.len();
    let n_u32 = u32::try_from(n).expect("token count exceeds u32");

    // Non-causal pooled embeddings need n_ubatch >= n_tokens, so size the context to this input.
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_u32))
        .with_n_batch(n_u32)
        .with_n_ubatch(n_u32)
        .with_embeddings(true)
        .with_pooling_type(spec.pooling)
        .with_n_threads(spec.n_threads)
        .with_n_threads_batch(spec.n_threads);

    let mut ctx = loaded
        .model
        .new_context(&loaded.backend, ctx_params)
        .map_err(|e| anyhow!("creating embedding context failed: {e}"))?;

    let mut batch = LlamaBatch::new(n, 1);
    for (pos, token) in tokens.iter().enumerate() {
        // logits=false: sequence pooling reads no per-token logits, as in llama.cpp's example.
        batch
            .add(*token, pos as i32, &[0], false)
            .map_err(|e| anyhow!("adding token to embedding batch failed: {e}"))?;
    }

    ctx.clear_kv_cache();
    ctx.decode(&mut batch)
        .map_err(|e| anyhow!("decoding embedding batch failed: {e}"))?;

    let raw = ctx
        .embeddings_seq_ith(0)
        .map_err(|e| anyhow!("reading pooled embedding failed: {e:?}"))?;

    if raw.len() != spec.dims {
        return Err(anyhow!(
            "embedding width {} != declared {} for {}",
            raw.len(),
            spec.dims,
            spec.model_id
        ));
    }
    // A stored NaN/inf scrambles ranking (sorts treat NaN as Equal), so refuse it.
    if !raw.iter().all(|x| x.is_finite()) {
        return Err(anyhow!(
            "embedding for {} contains non-finite values; refusing to store it",
            spec.model_id
        ));
    }

    Ok(l2_normalize(raw))
}

/// L2-normalise so cosine is a dot product; a zero vector is returned unchanged.
fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter().map(|x| x / norm).collect()
    } else {
        v.to_vec()
    }
}

impl GgufEmbeddingProvider {
    /// Shared body of `embed` / `embed_query`, which differ only in the task prefix.
    async fn embed_with_prefix(&self, text: &str, prefix: &'static str) -> Result<Vec<f32>> {
        let loaded = Arc::clone(&self.loaded);
        let spec = self.spec.clone();
        let model_path = self.model_path.clone();
        let owned = text.to_string();

        tokio::task::spawn_blocking(move || {
            // Lock across load+embed: contexts aren't Send and the load must not race itself.
            let mut guard = loaded.blocking_lock();
            if guard.is_none() {
                *guard = Some(load_sync(&spec, &model_path)?);
            }
            let loaded_ref = guard.as_ref().expect("just loaded");
            embed_sync(loaded_ref, &spec, &owned, prefix)
        })
        .await
        .map_err(|e| anyhow!("embedding spawn_blocking join error: {e}"))?
    }
}

#[async_trait]
impl EmbeddingProvider for GgufEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_with_prefix(text, self.spec.content_prefix).await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_with_prefix(text, self.spec.query_prefix).await
    }

    fn dimensions(&self) -> usize {
        self.spec.dims
    }

    fn model_id(&self) -> String {
        self.spec.model_id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_maps_names_and_defaults_to_nomic() {
        for name in ["", "gguf", "nomic-embed-text-v1.5"] {
            let s = EmbeddingModelSpec::resolve(name).unwrap();
            assert_eq!(s.model_id, "nomic-embed-text-v1.5");
            assert_eq!(s.dims, 768);
            assert!(matches!(s.pooling, LlamaPoolingType::Mean));
        }
        let bge = EmbeddingModelSpec::resolve("bge-base-en-v1.5").unwrap();
        assert_eq!(bge.dims, 768);
        assert!(matches!(bge.pooling, LlamaPoolingType::Cls));

        assert!(EmbeddingModelSpec::resolve("does-not-exist").is_err());
    }

    #[test]
    fn a_model_is_asymmetric_on_both_sides_or_neither() {
        for spec in EmbeddingModelSpec::all() {
            let doc = spec.content_prefix.is_empty();
            let query = spec.query_prefix.is_empty();
            assert_eq!(
                doc, query,
                "{} sets content_prefix={:?} but query_prefix={:?} -- an asymmetric \
                 model needs both, a symmetric one needs neither",
                spec.model_id, spec.content_prefix, spec.query_prefix
            );
        }
        // The asymmetric model must also use two different strings.
        let nomic = EmbeddingModelSpec::nomic_embed_text_v1_5();
        assert_ne!(nomic.content_prefix, nomic.query_prefix);
    }

    /// If this fires, pick a non-384 model rather than changing the number.
    #[test]
    fn no_gguf_model_shares_a_width_with_the_fastembed_provider() {
        const FASTEMBED_DIMS: usize = 384;
        for spec in EmbeddingModelSpec::all() {
            assert_ne!(
                spec.dims, FASTEMBED_DIMS,
                "{} is {}-dim, which collides with fastembed's width and makes a \
                 mixed store undetectable",
                spec.model_id, spec.dims
            );
        }
    }

    #[test]
    fn dimensions_are_declared_without_loading_a_model() {
        let p = GgufEmbeddingProvider::new(
            EmbeddingModelSpec::nomic_embed_text_v1_5(),
            Path::new("/nonexistent"),
        );
        assert_eq!(p.dimensions(), 768);
        assert_eq!(p.model_id(), "nomic-embed-text-v1.5");
    }

    #[test]
    fn l2_normalize_is_unit_length_and_zero_safe() {
        let out = l2_normalize(&[3.0, 4.0]);
        let norm = (out[0] * out[0] + out[1] * out[1]).sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "norm was {norm}");
        assert_eq!(l2_normalize(&[0.0, 0.0]), vec![0.0, 0.0]);
    }

    /// Needs the real GGUF at `$POND_EMBED_MODEL_DIR/nomic-embed-text-v1.5.Q8_0.gguf`.
    #[tokio::test]
    #[ignore]
    async fn live_embed_produces_a_unit_vector_and_ranks_related_text_higher() {
        let dir = std::env::var("POND_EMBED_MODEL_DIR")
            .expect("set POND_EMBED_MODEL_DIR to the folder holding the GGUF");
        let provider = GgufEmbeddingProvider::new(
            EmbeddingModelSpec::nomic_embed_text_v1_5(),
            Path::new(&dir),
        );

        let cat = provider.embed("the cat sat on the mat").await.unwrap();
        assert_eq!(cat.len(), 768);
        let norm = cat.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "not unit length: {norm}");

        // Same text via `embed_query` must differ, or the prefix isn't reaching the model.
        let as_query = provider
            .embed_query("the cat sat on the mat")
            .await
            .unwrap();
        assert_eq!(as_query.len(), 768);
        assert!(
            as_query.iter().zip(&cat).any(|(q, d)| (q - d).abs() > 1e-6),
            "embed_query returned the document vector -- the query prefix is not applied"
        );

        let kitten = provider.embed("a kitten rested on the rug").await.unwrap();
        let finance = provider
            .embed("quarterly interest rate policy")
            .await
            .unwrap();
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(
            dot(&cat, &kitten) > dot(&cat, &finance),
            "related text should score higher than unrelated"
        );
    }
}
