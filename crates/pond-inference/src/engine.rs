//! llama-cpp-2 engine: shared backend, model load/unload, and the model slot `provider.rs` uses.

use anyhow::{Context, Result};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatTemplate, LlamaModel};
use llama_cpp_2::LogOptions;
use pond_core::models::domain::model_capabilities::ModelCapabilities;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once, OnceLock, RwLock as StdRwLock};
use tokio::sync::Mutex;

/// A model loaded into memory with its chat template and capabilities.
pub(crate) struct LoadedModel {
    pub model: LlamaModel,
    pub model_id: String,
    pub chat_template: LlamaChatTemplate,
    pub capabilities: ModelCapabilities,
    /// KV-cache context kept across calls so only the prompt delta is prefilled.
    /// Borrows `model`, so `Drop` clears it first; clear it before assigning a new `model`.
    pub cached_ctx: Option<CachedInferenceContext>,
}

impl Drop for LoadedModel {
    fn drop(&mut self) {
        // Fields drop in declaration order, which would free `model` before the context using it.
        self.cached_ctx = None;
    }
}

/// Persistent context with its token history, for KV-cache prefix reuse.
///
/// # Safety
///
/// `ctx` really borrows the sibling `model` but is stored as `'static`, so `cached_ctx` must be
/// cleared before that model drops. All access goes through the model mutex.
pub(crate) struct CachedInferenceContext {
    /// KV-cache context; its `'static` really borrows `LoadedModel::model`.
    pub ctx: llama_cpp_2::context::LlamaContext<'static>,
    /// Tokens currently prefilled in the KV cache (for prefix matching).
    pub tokens_in_cache: Vec<llama_cpp_2::token::LlamaToken>,
}

// SAFETY: only accessed through the `Mutex<Option<LoadedModel>>`, and llama.cpp contexts are
// safe to use from any thread as long as calls are serialised.
unsafe impl Send for CachedInferenceContext {}
unsafe impl Sync for CachedInferenceContext {}

/// Held strongly forever: nothing may drop a `LlamaBackend`; see [`get_or_init_backend`].
static BACKEND: OnceLock<Arc<LlamaBackend>> = OnceLock::new();

/// Set the llama.cpp log bridge exactly once, no matter how many callers race.
static LOG_BRIDGE: Once = Once::new();

/// The process-wide backend, initialised directly so GIAP never enters `LlamaBackend::init()`'s
/// CAS: that flag is shared with Goose, which panics if it loses it. Never drop the handle: its
/// `Drop` resets the flag and frees the backend under Goose.
pub(crate) fn get_or_init_backend() -> Result<Arc<LlamaBackend>> {
    Ok(BACKEND
        .get_or_init(|| {
            // SAFETY: idempotent (Goose may call it too) and touches only ggml's global setup.
            unsafe { llama_cpp_sys_2::llama_backend_init() };
            LOG_BRIDGE.call_once(|| llama_cpp_2::send_logs_to_tracing(LogOptions::default()));
            tracing::info!(
                "llama backend ready (initialised directly; the llama-cpp-2 init flag is \
                 left to Goose so its runtime can never lose the race)"
            );
            // `LlamaBackend` is a field-less token asserting the backend is up, which it now is.
            Arc::new(LlamaBackend {})
        })
        .clone())
}

/// Model slot shared between the engine and `spawn_blocking` tasks.
pub(crate) type ModelSlot = Arc<Mutex<Option<LoadedModel>>>;

/// In-process GGUF inference engine: the shared backend plus at most one loaded model.
pub struct LlamaCppEngine {
    model: ModelSlot,
    backend: Arc<LlamaBackend>,
    data_dir: PathBuf,
    /// Copy of the model's capabilities, readable without the mutex generation holds.
    capabilities: Arc<StdRwLock<ModelCapabilities>>,
}

impl LlamaCppEngine {
    /// New engine with no model loaded; GGUFs are expected under `{data_dir}/models/gguf/`.
    pub fn new(data_dir: &Path) -> Result<Self> {
        let backend = get_or_init_backend()?;
        Ok(Self {
            model: Arc::new(Mutex::new(None)),
            backend,
            data_dir: data_dir.to_path_buf(),
            capabilities: Arc::new(StdRwLock::new(ModelCapabilities::default())),
        })
    }

    /// Load `{data_dir}/models/gguf/{model_id}[.gguf]`, replacing any loaded model.
    pub async fn load_model(
        &self,
        model_id: &str,
        n_gpu_layers: u32,
        flash_attention: bool,
    ) -> Result<()> {
        let model_path = self.resolve_model_path(model_id)?;
        let backend = self.backend.clone();
        let model_id_owned = model_id.to_string();

        let loaded = tokio::task::spawn_blocking(move || {
            load_model_sync(
                &backend,
                &model_path,
                &model_id_owned,
                n_gpu_layers,
                flash_attention,
            )
        })
        .await
        .context("model loading task panicked")??;

        *self
            .capabilities
            .write()
            .expect("capabilities lock poisoned") = loaded.capabilities.clone();

        let mut guard = self.model.lock().await;
        *guard = Some(loaded);
        Ok(())
    }

    /// Unload the current model, freeing all GPU/CPU memory.
    pub async fn unload_model(&self) {
        *self
            .capabilities
            .write()
            .expect("capabilities lock poisoned") = ModelCapabilities::default();

        let mut guard = self.model.lock().await;
        if let Some(loaded) = guard.as_mut() {
            // Drop cached context first — it borrows from the model.
            loaded.cached_ctx = None;
            tracing::info!("unloading model");
        }
        *guard = None;
    }

    pub async fn is_loaded(&self) -> bool {
        self.model.lock().await.is_some()
    }

    /// The name of the currently loaded model, or `"none"`.
    pub async fn model_name(&self) -> String {
        self.model
            .lock()
            .await
            .as_ref()
            .map(|m| m.model_id.clone())
            .unwrap_or_else(|| "none".to_string())
    }

    pub async fn model_capabilities(&self) -> ModelCapabilities {
        self.model
            .lock()
            .await
            .as_ref()
            .map(|m| m.capabilities.clone())
            .unwrap_or_default()
    }

    /// Capabilities cached at load; sync and never waits on the model mutex held during generation.
    pub fn cached_capabilities(&self) -> ModelCapabilities {
        self.capabilities
            .read()
            .expect("capabilities lock poisoned")
            .clone()
    }

    pub(crate) fn model_slot(&self) -> ModelSlot {
        Arc::clone(&self.model)
    }

    pub(crate) fn backend_arc(&self) -> Arc<LlamaBackend> {
        Arc::clone(&self.backend)
    }

    /// Invalidate the in-memory KV cache (e.g. when settings change the prompt).
    pub async fn invalidate_kv_cache(&self) {
        let mut guard = self.model.lock().await;
        if let Some(loaded) = guard.as_mut() {
            loaded.cached_ctx = None;
            tracing::info!("KV cache invalidated");
        }
    }

    fn resolve_model_path(&self, model_id: &str) -> Result<PathBuf> {
        let gguf_dir = self.data_dir.join("models").join("gguf");

        let with_ext = if model_id.ends_with(".gguf") {
            gguf_dir.join(model_id)
        } else {
            gguf_dir.join(format!("{}.gguf", model_id))
        };

        if with_ext.exists() {
            return Ok(with_ext);
        }

        let abs = Path::new(model_id);
        if abs.is_absolute() && abs.exists() {
            return Ok(abs.to_path_buf());
        }

        anyhow::bail!(
            "GGUF model not found: tried '{}' and '{}'",
            with_ext.display(),
            model_id
        );
    }
}

/// Synchronous model loading -- runs inside `spawn_blocking`.
fn load_model_sync(
    backend: &LlamaBackend,
    model_path: &Path,
    model_id: &str,
    n_gpu_layers: u32,
    flash_attention: bool,
) -> Result<LoadedModel> {
    tracing::info!(
        model_id,
        path = %model_path.display(),
        n_gpu_layers,
        flash_attention,
        "loading GGUF model"
    );

    let params = LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers);

    let model = LlamaModel::load_from_file(backend, model_path, &params)
        .map_err(|e| anyhow::anyhow!("failed to load model: {}", e))?;

    let chat_template = match model.chat_template(None) {
        Ok(t) => t,
        Err(_) => {
            tracing::warn!("model has no embedded chat template, falling back to chatml");
            LlamaChatTemplate::new("chatml")
                .map_err(|e| anyhow::anyhow!("failed to create fallback chat template: {}", e))?
        }
    };

    let capabilities = ModelCapabilities::from_model_name(model_id);

    tracing::info!(
        model_id,
        n_ctx_train = model.n_ctx_train(),
        n_layer = model.n_layer(),
        thinking = capabilities.thinking,
        tool_calling = capabilities.tool_calling,
        "model loaded successfully"
    );

    // Flash attention applies at context creation; only log the intent here.
    if flash_attention {
        tracing::info!("flash attention will be enabled for inference contexts");
    }

    Ok(LoadedModel {
        model,
        model_id: model_id.to_string(),
        chat_template,
        capabilities,
        cached_ctx: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn backend_singleton_returns_same_arc() {
        let a = get_or_init_backend().expect("init");
        let b = get_or_init_backend().expect("init");
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn resolve_model_path_appends_gguf_extension() {
        let engine = LlamaCppEngine {
            model: Arc::new(Mutex::new(None)),
            backend: get_or_init_backend().expect("init"),
            data_dir: PathBuf::from("/tmp/test-data"),
            capabilities: Arc::new(StdRwLock::new(ModelCapabilities::default())),
        };

        let err = engine.resolve_model_path("nonexistent-model").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nonexistent-model.gguf"),
            "error should mention .gguf path, got: {}",
            msg
        );
    }

    #[test]
    fn model_capabilities_from_known_model() {
        let caps = ModelCapabilities::from_model_name("gemma-4-E2B-it-Q4_K_M.gguf");
        assert!(caps.thinking);
        assert!(caps.tool_calling);
        assert!(caps.vision);
    }
}
