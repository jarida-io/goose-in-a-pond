//! GGUF-backed [`EmbeddingProvider`] over llama.cpp. Coexists with Goose's llama.cpp via
//! [`get_or_init_backend`], not load order.
//!
//! [`get_or_init_backend`]: crate::engine::get_or_init_backend

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::token::LlamaToken;
use pond_core::models::domain::embedding_models::{self, EmbeddingModel, GgufFile};
use pond_core::models::domain::gpu_gate::gpu_gate;
use pond_core::models::ports::embedding::EmbeddingProvider;
use std::num::NonZeroU32;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Per-text token cap for pathological inputs; `min`'d with the model's trained context.
const MAX_EMBED_TOKENS: usize = 2048;

/// P0's Orin setting for EmbeddingGemma 2: a 512-token ubatch and at most 16 sequences, since a
/// 1,024-token, 64-sequence context cost 610 MB on the CPU, mostly buffers sized by the ubatch.
const EG2_CONTEXT_TOKENS: u32 = 512;
const EG2_MAX_SEQS: u32 = 16;

/// P0's measured CPU setting on the Orin (41 ms a query at 4 threads, 69 at 2); the slot never
/// embeds beside a turn, and a turn's own query embed runs before its prefill.
const EG2_THREADS: i32 = 4;

/// Where a model's layers and compute run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Cpu,
    /// Every layer on the GPU, the token table mapped rather than pinned (`no_host`), which kept
    /// 136 MB out of NvMap in P0.
    Gpu,
}

/// How a model's llama.cpp contexts are made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextPlan {
    /// A fresh context sized to each text, so nothing but the weights stays resident.
    PerText,
    /// One context kept across calls; a decode packs up to `max_seqs` texts into `n_ctx` tokens.
    Retained { n_ctx: u32, max_seqs: u32 },
}

/// Everything that makes one embedding model's vectors incompatible with another's.
#[derive(Clone, Debug)]
pub struct EmbeddingModelSpec {
    /// The catalogue row's name, which `active_embedding_model` stores.
    pub name: &'static str,
    /// Stable identifier stamped onto every vector this model produces.
    pub model_id: String,
    /// GGUF filename expected under `{data_dir}/models/embedding/`.
    pub filename: String,
    /// The pooled output width, asserted against the loaded model's `n_embd_out`.
    pub output_dims: usize,
    /// The stored width: the leading `dims` of the output, re-normalised (a Matryoshka cut).
    pub dims: usize,
    /// Sequence pooling (nomic mean, bge CLS); the wrong one silently yields worse vectors.
    pub pooling: LlamaPoolingType,
    /// Task prefix prepended to a stored DOCUMENT (nomic: `search_document: `, bge: none).
    pub content_prefix: &'static str,
    /// Task prefix for a QUERY, used by `embed_query` (nomic: `search_query: `, bge: none).
    pub query_prefix: &'static str,
    pub placement: Placement,
    /// ggml threads; small because llama.cpp's default 4 starves chat prefill on a 6-core Orin.
    pub n_threads: i32,
    pub context: ContextPlan,
}

impl EmbeddingModelSpec {
    /// The run settings for a catalogue row's GGUF; identity, width and file come from the row.
    fn for_row(
        model: &EmbeddingModel,
        file: &GgufFile,
        pooling: LlamaPoolingType,
        content_prefix: &'static str,
        query_prefix: &'static str,
    ) -> Self {
        Self {
            name: model.name,
            model_id: model.model_id.to_string(),
            filename: file.filename.to_string(),
            output_dims: model.dims,
            dims: model.dims,
            pooling,
            content_prefix,
            query_prefix,
            // The GPU belongs to the chat model unless the factory finds room for this one.
            placement: Placement::Cpu,
            n_threads: 2,
            context: ContextPlan::PerText,
        }
    }

    /// Default: `nomic-embed-text-v1.5`, 768-dim, mean-pooled, retrieval-tuned.
    pub fn nomic_embed_text_v1_5() -> Self {
        Self::for_row(
            &embedding_models::NOMIC_EMBED_TEXT_V1_5,
            &embedding_models::NOMIC_EMBED_TEXT_V1_5_Q8_0,
            LlamaPoolingType::Mean,
            // nomic needs a task prefix, and a different one per side.
            "search_document: ",
            "search_query: ",
        )
    }

    /// `bge-base-en-v1.5`, 768-dim, CLS-pooled: the fallback if nomic won't load. Never add a
    /// 384-dim model: width is what tells these vectors apart from fastembed's 384-dim ones.
    pub fn bge_base_en_v1_5() -> Self {
        Self::for_row(
            &embedding_models::BGE_BASE_EN_V1_5,
            &embedding_models::BGE_BASE_EN_V1_5_Q8_0,
            LlamaPoolingType::Cls,
            "",
            "",
        )
    }

    /// EmbeddingGemma 2's text encoder: 768 wide out, stored as its leading 256, which equal or
    /// beat all 768 on every P0 set. The prompts are the model card's, word for word.
    pub fn embeddinggemma_2() -> Self {
        Self {
            output_dims: 768,
            n_threads: EG2_THREADS,
            context: ContextPlan::Retained {
                n_ctx: EG2_CONTEXT_TOKENS,
                max_seqs: EG2_MAX_SEQS,
            },
            ..Self::for_row(
                &embedding_models::EMBEDDINGGEMMA_2,
                &embedding_models::EMBEDDINGGEMMA_2_Q8_0,
                LlamaPoolingType::Mean,
                "title: none | text: ",
                "task: search result | query: ",
            )
        }
    }

    pub fn with_placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    /// Resolve a settings model name ("" and "gguf" mean nomic). An unknown name is usually a
    /// leftover fastembed name (the setting is shared), so callers fall back to the default.
    pub fn resolve(name: &str) -> Result<Self> {
        match name {
            "" | "gguf" => Ok(Self::nomic_embed_text_v1_5()),
            n if n == embedding_models::NOMIC_EMBED_TEXT_V1_5.name => {
                Ok(Self::nomic_embed_text_v1_5())
            }
            n if n == embedding_models::BGE_BASE_EN_V1_5.name => Ok(Self::bge_base_en_v1_5()),
            n if n == embedding_models::EMBEDDINGGEMMA_2.name => Ok(Self::embeddinggemma_2()),
            other => Err(anyhow!(
                "unknown GGUF embedding model: '{other}'. \
                 Supported: nomic-embed-text-v1.5, bge-base-en-v1.5, embeddinggemma-2"
            )),
        }
    }

    /// How many texts one decode takes.
    fn max_batch(&self) -> usize {
        match self.context {
            ContextPlan::PerText => 1,
            ContextPlan::Retained { max_seqs, .. } => max_seqs as usize,
        }
    }

    /// Every GGUF embedding model this crate will load.
    #[cfg(test)]
    fn all() -> Vec<Self> {
        vec![
            Self::nomic_embed_text_v1_5(),
            Self::bge_base_en_v1_5(),
            Self::embeddinggemma_2(),
        ]
    }
}

/// Declared before `model`: fields drop in order, so the kept context is freed before the model
/// it points into.
struct Loaded {
    retained: Option<LlamaContext<'static>>,
    model: Arc<LlamaModel>,
    backend: Arc<LlamaBackend>,
    /// Most tokens one text keeps.
    max_tokens: usize,
}

// SAFETY: `LlamaContext` is `!Send` only for its raw pointer. A llama.cpp context has no thread
// affinity, and every access to a `Loaded` holds the provider's mutex, so none is concurrent.
unsafe impl Send for Loaded {}

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

    pub fn placement(&self) -> Placement {
        self.spec.placement
    }
}

/// A width other than declared would poison every stored vector, so loading refuses it.
fn check_output_width(spec: &EmbeddingModelSpec, n_embd_out: i32) -> Result<()> {
    let actual = usize::try_from(n_embd_out).unwrap_or(0);
    if actual != spec.output_dims {
        return Err(anyhow!(
            "embedding model {} reports n_embd_out={actual} but the spec declares {} \
             dimensions — the vector space would be wrong. Refusing to load.",
            spec.model_id,
            spec.output_dims
        ));
    }
    Ok(())
}

fn model_params(placement: Placement) -> LlamaModelParams {
    match placement {
        Placement::Cpu => LlamaModelParams::default().with_n_gpu_layers(0),
        Placement::Gpu => LlamaModelParams::default()
            .with_n_gpu_layers(u32::MAX)
            .with_no_host(true),
    }
}

/// Non-causal pooled embeddings need n_ubatch >= the tokens of one decode.
fn context_params(spec: &EmbeddingModelSpec, n_tokens: u32) -> LlamaContextParams {
    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_tokens))
        .with_n_batch(n_tokens)
        .with_n_ubatch(n_tokens)
        .with_embeddings(true)
        .with_pooling_type(spec.pooling)
        .with_n_threads(spec.n_threads)
        .with_n_threads_batch(spec.n_threads);
    let ContextPlan::Retained { max_seqs, .. } = spec.context else {
        return params;
    };
    let params = params.with_n_seq_max(max_seqs).with_kv_unified(true);
    match spec.placement {
        // The CPU fallback exists to keep the encoder out of NvMap; op offload would put it back.
        Placement::Cpu => params.with_op_offload(false).with_offload_kqv(false),
        Placement::Gpu => params,
    }
}

/// Load the model, and its kept context if it has one; blocking, so run it in `spawn_blocking`.
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

    let model = LlamaModel::load_from_file(&backend, model_path, &model_params(spec.placement))
        .map_err(|e| {
            anyhow!(
                "failed to load embedding model {}: {e}",
                model_path.display()
            )
        })?;
    check_output_width(spec, model.n_embd_out())?;
    let model = Arc::new(model);

    let trained = (model.n_ctx_train() as usize).max(1);
    let (max_tokens, retained) = match spec.context {
        ContextPlan::PerText => (trained.min(MAX_EMBED_TOKENS), None),
        ContextPlan::Retained { n_ctx, .. } => {
            let ctx = model
                .new_context(&backend, context_params(spec, n_ctx))
                .map_err(|e| anyhow!("creating the embedding context failed: {e}"))?;
            // SAFETY: `ctx` borrows the model inside the `Arc`, whose address is stable while any
            // handle lives. The erased lifetime is re-tied to `Loaded::model`, which drops after
            // `Loaded::retained` by field order, so the context never outlives its model.
            let ctx =
                unsafe { std::mem::transmute::<LlamaContext<'_>, LlamaContext<'static>>(ctx) };
            (trained.min(n_ctx as usize), Some(ctx))
        }
    };

    tracing::info!(
        model_id = %spec.model_id,
        dims = spec.dims,
        placement = ?spec.placement,
        max_tokens,
        retained = retained.is_some(),
        "GGUF embedding model loaded"
    );

    Ok(Loaded {
        retained,
        model,
        backend,
        max_tokens,
    })
}

/// `prefix` + `text` as tokens, cut to `max_tokens`.
fn tokenise(
    model: &LlamaModel,
    spec: &EmbeddingModelSpec,
    text: &str,
    prefix: &str,
    max_tokens: usize,
) -> Result<Vec<LlamaToken>> {
    let mut tokens = model
        .str_to_token(&format!("{prefix}{text}"), AddBos::Always)
        .map_err(|e| anyhow!("tokenising text for embedding failed: {e}"))?;
    if tokens.is_empty() {
        return Err(anyhow!("text produced no tokens to embed"));
    }
    if tokens.len() > max_tokens {
        // Truncated rows count as embedded and are never revisited, so warn.
        tracing::warn!(
            model_id = %spec.model_id,
            tokens = tokens.len(),
            kept = max_tokens,
            "embedding input truncated; the discarded tail is not searchable"
        );
        tokens.truncate(max_tokens);
    }
    Ok(tokens)
}

/// Consecutive runs of texts that fit one decode: at most `max_seqs` texts and `budget` tokens.
/// A text longer than `budget` must already have been cut to it.
fn packs(lens: &[usize], budget: usize, max_seqs: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let (mut start, mut tokens) = (0, 0);
    for (i, &len) in lens.iter().enumerate() {
        if i > start && (tokens + len > budget || i - start == max_seqs) {
            out.push(start..i);
            (start, tokens) = (i, 0);
        }
        tokens += len;
    }
    if start < lens.len() {
        out.push(start..lens.len());
    }
    out
}

/// The stored vector: the leading `dims` of the pooled output, L2-normalised.
fn finish(spec: &EmbeddingModelSpec, raw: &[f32]) -> Result<Vec<f32>> {
    if raw.len() != spec.output_dims {
        return Err(anyhow!(
            "embedding width {} != declared {} for {}",
            raw.len(),
            spec.output_dims,
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
    Ok(l2_normalize(&raw[..spec.dims]))
}

/// Decode `seqs` as one batch, one sequence each, and read each pooled vector. Blocking.
fn decode_into(
    ctx: &mut LlamaContext<'_>,
    spec: &EmbeddingModelSpec,
    seqs: &[Vec<LlamaToken>],
    out: &mut Vec<Vec<f32>>,
) -> Result<()> {
    let total = seqs.iter().map(Vec::len).sum();
    let mut batch = LlamaBatch::new(total, 1);
    for (seq, tokens) in seqs.iter().enumerate() {
        let seq = i32::try_from(seq).context("sequence id")?;
        for (pos, token) in tokens.iter().enumerate() {
            // logits=false: pooled contexts output every token anyway, as in llama.cpp's example.
            batch
                .add(*token, i32::try_from(pos)?, &[seq], false)
                .map_err(|e| anyhow!("adding token to embedding batch failed: {e}"))?;
        }
    }

    let _gpu = gpu_gate().compute_blocking();
    ctx.clear_kv_cache();
    ctx.decode(&mut batch)
        .map_err(|e| anyhow!("decoding embedding batch failed: {e}"))?;
    for seq in 0..seqs.len() {
        let raw = ctx
            .embeddings_seq_ith(i32::try_from(seq)?)
            .map_err(|e| anyhow!("reading pooled embedding failed: {e:?}"))?;
        out.push(finish(spec, raw)?);
    }
    Ok(())
}

/// Embed each text with `prefix`, one vector per text in order. Blocking.
fn embed_sync(
    loaded: &mut Loaded,
    spec: &EmbeddingModelSpec,
    texts: &[String],
    prefix: &str,
) -> Result<Vec<Vec<f32>>> {
    let tokenised = texts
        .iter()
        .map(|t| tokenise(&loaded.model, spec, t, prefix, loaded.max_tokens))
        .collect::<Result<Vec<_>>>()?;
    let mut out = Vec::with_capacity(texts.len());

    let Some(ctx) = loaded.retained.as_mut() else {
        for tokens in &tokenised {
            let n = u32::try_from(tokens.len()).context("token count exceeds u32")?;
            let mut ctx = loaded
                .model
                .new_context(&loaded.backend, context_params(spec, n))
                .map_err(|e| anyhow!("creating embedding context failed: {e}"))?;
            decode_into(&mut ctx, spec, std::slice::from_ref(tokens), &mut out)?;
        }
        return Ok(out);
    };

    let lens: Vec<usize> = tokenised.iter().map(Vec::len).collect();
    for pack in packs(&lens, loaded.max_tokens, spec.max_batch()) {
        decode_into(ctx, spec, &tokenised[pack], &mut out)?;
    }
    Ok(out)
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
    /// Shared body of every embed, which differ only in the task prefix.
    async fn embed_texts(&self, texts: Vec<String>, prefix: &'static str) -> Result<Vec<Vec<f32>>> {
        let loaded = Arc::clone(&self.loaded);
        let spec = self.spec.clone();
        let model_path = self.model_path.clone();

        tokio::task::spawn_blocking(move || {
            // Held across load and embed: the load must not race itself, nor a context be shared.
            let mut guard = loaded.blocking_lock();
            if guard.is_none() {
                *guard = Some(load_sync(&spec, &model_path)?);
            }
            let loaded = guard.as_mut().expect("just loaded");
            embed_sync(loaded, &spec, &texts, prefix)
        })
        .await
        .map_err(|e| anyhow!("embedding spawn_blocking join error: {e}"))?
    }

    async fn embed_one(&self, text: &str, prefix: &'static str) -> Result<Vec<f32>> {
        self.embed_texts(vec![text.to_string()], prefix)
            .await?
            .pop()
            .ok_or_else(|| anyhow!("the embedder returned no vector"))
    }
}

#[async_trait]
impl EmbeddingProvider for GgufEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_one(text, self.spec.content_prefix).await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_one(text, self.spec.query_prefix).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_texts(texts.to_vec(), self.spec.content_prefix)
            .await
    }

    fn max_batch(&self) -> usize {
        self.spec.max_batch()
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

        let eg2 = EmbeddingModelSpec::resolve("embeddinggemma-2").unwrap();
        assert_eq!(eg2.model_id, "embeddinggemma-2-256");
        assert!(matches!(eg2.pooling, LlamaPoolingType::Mean));

        assert!(EmbeddingModelSpec::resolve("does-not-exist").is_err());
    }

    /// The model card's task prompts, word for word; P0's recall was measured with exactly these.
    #[test]
    fn embeddinggemma_2_uses_the_cards_prompts() {
        let eg2 = EmbeddingModelSpec::embeddinggemma_2();
        assert_eq!(eg2.query_prefix, "task: search result | query: ");
        assert_eq!(eg2.content_prefix, "title: none | text: ");
    }

    /// llama.cpp reads pooled vectors at `n_embd_out` (768); `n_embd` (512) is the hidden width.
    #[test]
    fn the_width_check_reads_n_embd_out() {
        let eg2 = EmbeddingModelSpec::embeddinggemma_2();
        assert_eq!((eg2.output_dims, eg2.dims), (768, 256));
        assert!(check_output_width(&eg2, 768).is_ok());
        assert!(
            check_output_width(&eg2, 512).is_err(),
            "n_embd is not the output"
        );
        let nomic = EmbeddingModelSpec::nomic_embed_text_v1_5();
        assert_eq!(
            nomic.output_dims, nomic.dims,
            "nomic stores what it outputs"
        );
        assert!(check_output_width(&nomic, 768).is_ok());
    }

    #[test]
    fn a_matryoshka_cut_keeps_the_leading_dims_at_unit_length() {
        let eg2 = EmbeddingModelSpec::embeddinggemma_2();
        let mut raw = vec![0.0_f32; 768];
        raw[0] = 3.0;
        raw[1] = 4.0;
        raw[255] = 12.0;
        raw[256] = 1000.0;
        raw[767] = -1000.0;
        let v = finish(&eg2, &raw).unwrap();
        assert_eq!(v.len(), 256);
        assert!((v[0] - 3.0 / 13.0).abs() < 1e-6);
        assert!((v[1] - 4.0 / 13.0).abs() < 1e-6);
        assert!((v[255] - 12.0 / 13.0).abs() < 1e-6);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "norm was {norm}");

        assert!(
            finish(&eg2, &raw[..512]).is_err(),
            "the output width is checked"
        );
        raw[3] = f32::NAN;
        assert!(finish(&eg2, &raw).is_err());
    }

    #[test]
    fn only_embeddinggemma_2_keeps_a_context_and_batches() {
        let eg2 = EmbeddingModelSpec::embeddinggemma_2();
        assert_eq!(
            eg2.context,
            ContextPlan::Retained {
                n_ctx: 512,
                max_seqs: 16
            }
        );
        assert_eq!(eg2.max_batch(), 16);
        for spec in [
            EmbeddingModelSpec::nomic_embed_text_v1_5(),
            EmbeddingModelSpec::bge_base_en_v1_5(),
        ] {
            assert_eq!(spec.context, ContextPlan::PerText, "{}", spec.model_id);
            assert_eq!(spec.max_batch(), 1);
            assert_eq!(spec.placement, Placement::Cpu);
        }
        let p = GgufEmbeddingProvider::new(eg2, Path::new("/nonexistent"));
        assert_eq!(p.max_batch(), 16);
        assert_eq!(p.dimensions(), 256);
        assert_eq!(
            p.placement(),
            Placement::Cpu,
            "the GPU is the factory's call"
        );
    }

    #[test]
    fn packs_respect_the_token_budget_and_the_sequence_cap() {
        assert!(packs(&[], 512, 16).is_empty());
        assert_eq!(packs(&[30; 16], 512, 16), vec![0..16]);
        assert_eq!(packs(&[30; 17], 512, 16), vec![0..16, 16..17]);
        assert_eq!(packs(&[200, 200, 200], 512, 16), vec![0..2, 2..3]);
        assert_eq!(packs(&[512, 1, 512], 512, 16), vec![0..1, 1..2, 2..3]);
        assert_eq!(packs(&[100; 5], 512, 2), vec![0..2, 2..4, 4..5]);
        for pack in packs(&[90, 300, 120, 512, 7, 7, 400], 512, 3) {
            assert!(pack.len() <= 3);
        }
    }

    #[test]
    fn a_retained_cpu_context_never_offloads() {
        let eg2 = EmbeddingModelSpec::embeddinggemma_2();
        let cpu = context_params(&eg2, 512);
        assert_eq!(
            (cpu.n_ctx().map(|n| n.get()), cpu.n_ubatch()),
            (Some(512), 512)
        );
        assert_eq!(cpu.n_seq_max(), 16);
        assert!(cpu.kv_unified());
        assert!(!cpu.op_offload());
        let gpu = context_params(&eg2.with_placement(Placement::Gpu), 512);
        assert!(gpu.op_offload());
        assert!(model_params(Placement::Gpu).no_host());
        assert!(!model_params(Placement::Cpu).no_host());
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
            if !doc {
                assert_ne!(spec.content_prefix, spec.query_prefix, "{}", spec.model_id);
            }
        }
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

    /// The catalogue downloads the file this crate loads; a second copy of either would drift.
    #[test]
    fn every_spec_is_its_catalogue_row() {
        let ggufs: Vec<_> = embedding_models::EMBEDDING_MODELS
            .iter()
            .filter(|m| m.gguf().is_some())
            .collect();
        assert_eq!(ggufs.len(), EmbeddingModelSpec::all().len());
        for model in ggufs {
            let spec = EmbeddingModelSpec::resolve(model.name).unwrap();
            assert_eq!(spec.name, model.name);
            assert_eq!(spec.model_id, model.model_id);
            assert_eq!(spec.dims, model.dims);
            assert_eq!(
                Some(spec.filename.as_str()),
                model.gguf().map(|f| f.filename)
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

    /// The ggml-org file checked against P0's own conversion (decision 11), and P0's 25 pctx
    /// queries with their normalised 768-wide reference vectors (float32, row-major) beside it.
    const EG2_LIVE_DIR: &str = "/private/tmp/claude-501/-Users-jerry-Documents-Jarida-goose-in-a-pond/64de05e3-726e-4d53-ad32-c57617693f1d/scratchpad/eg2check";

    fn dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    /// Every pctx query through the provider's query path, against P0's reference cut to 256 and
    /// re-normalised; then a batch of 16 against the same texts one at a time.
    async fn eg2_matches_p0(placement: Placement) -> Option<(f32, f32)> {
        let dir = Path::new(EG2_LIVE_DIR);
        let model_dir = dir.join("ggml-org");
        if !model_dir.join("embeddinggemma-2-Q8_0.gguf").exists() {
            eprintln!(
                "skipped: no EmbeddingGemma 2 GGUF under {}",
                model_dir.display()
            );
            return None;
        }
        let spec = EmbeddingModelSpec::embeddinggemma_2().with_placement(placement);
        let query_prefix = spec.query_prefix;
        let provider = GgufEmbeddingProvider::new(spec, &model_dir);

        let lines = std::fs::read_to_string(dir.join("ref/pctx.queries.eg2.txt")).unwrap();
        let reference: Vec<f32> = std::fs::read(dir.join("ref/pctx.queries.eg2-q8_0-cpu.f32"))
            .unwrap()
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let queries: Vec<&str> = lines
            .lines()
            .map(|l| l.strip_prefix(query_prefix).expect("a prefixed P0 query"))
            .collect();
        assert_eq!(queries.len(), 25);
        assert_eq!(reference.len(), 25 * 768);

        let mut min_single = f32::INFINITY;
        for (i, query) in queries.iter().enumerate() {
            let got = provider.embed_query(query).await.unwrap();
            assert_eq!(got.len(), 256);
            let want = l2_normalize(&reference[i * 768..i * 768 + 256]);
            let cos = dot(&got, &want);
            assert!(cos >= 0.999, "query {i} ({query}): cosine {cos:.5} to P0's");
            min_single = min_single.min(cos);
        }

        let texts: Vec<String> = queries.iter().take(16).map(|q| q.to_string()).collect();
        let batched = provider.embed_batch(&texts).await.unwrap();
        assert_eq!(batched.len(), 16);
        let mut by_text = Vec::new();
        for (text, b) in texts.iter().zip(&batched) {
            let alone = provider.embed(text).await.unwrap();
            by_text.push(dot(&alone, b));
        }
        eprintln!("{placement:?} batched vs one-by-one: {by_text:.6?}");
        let min_batch = by_text.iter().copied().fold(f32::INFINITY, f32::min);
        // llama.cpp's repacked CPU matmul takes another tile path as a batch's rows change, which
        // moves the fourth decimal (0.99984 on the Mac); P0's CPU-vs-CUDA agreement is 0.9987.
        let floor = match placement {
            Placement::Gpu => 0.9999,
            Placement::Cpu => 0.9995,
        };
        assert!(
            min_batch >= floor,
            "batched cosine {min_batch:.6} to one-by-one"
        );
        Some((min_single, min_batch))
    }

    #[tokio::test]
    #[ignore]
    async fn live_embeddinggemma_2_matches_p0_on_the_cpu() {
        if let Some((single, batch)) = eg2_matches_p0(Placement::Cpu).await {
            eprintln!("EG2 cpu: min cosine to P0 {single:.5}, batch vs one-by-one {batch:.6}");
        }
    }

    #[tokio::test]
    #[ignore]
    async fn live_embeddinggemma_2_matches_p0_on_the_gpu() {
        if let Some((single, batch)) = eg2_matches_p0(Placement::Gpu).await {
            eprintln!("EG2 gpu: min cosine to P0 {single:.5}, batch vs one-by-one {batch:.6}");
        }
    }
}
