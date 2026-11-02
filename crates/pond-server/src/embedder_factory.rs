//! Builds the Memory-role embedder: fastembed for its ONNX models, llama.cpp for a pinned GGUF.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_api::AppState;
use pond_core::models::domain::embedding_models::{EmbeddingEngine, EmbeddingModel};
use pond_core::models::ports::embedding::{EmbedderFactory, EmbeddingProvider};

/// ONNX Runtime can hang rather than fail when its version does not match.
const FASTEMBED_INIT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct ServerEmbedderFactory {
    /// `<data_dir>/models/embedding`: fastembed's cache and the GGUF downloads.
    dir: PathBuf,
    /// Set once `AppState` exists, so a missing GGUF is fetched by the models pipeline.
    state: OnceLock<Weak<AppState>>,
}

impl ServerEmbedderFactory {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            state: OnceLock::new(),
        }
    }

    pub fn attach(&self, state: &Arc<AppState>) {
        let _ = self.state.set(Arc::downgrade(state));
    }

    async fn fastembed(&self, model: &EmbeddingModel) -> Result<Arc<dyn EmbeddingProvider>> {
        let name = model.name.to_string();
        let cache = self.dir.clone();
        let built = tokio::time::timeout(
            FASTEMBED_INIT_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                pond_infra::fastembed_embedding::FastembedEmbeddingProvider::new(&name, Some(cache))
            }),
        )
        .await
        .map_err(|_| {
            anyhow!(
                "fastembed did not start within {} s; ONNX Runtime may be the wrong version",
                FASTEMBED_INIT_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| anyhow!("fastembed's loader stopped: {e}"))??;
        Ok(Arc::new(built))
    }

    #[cfg(feature = "local-inference")]
    async fn gguf(&self, model: &EmbeddingModel) -> Result<Arc<dyn EmbeddingProvider>> {
        use pond_core::models::domain::device_budget::embedder_on_gpu;
        use pond_inference::{EmbeddingModelSpec, Placement};

        let file = model
            .gguf()
            .ok_or_else(|| anyhow!("{} has no GGUF file", model.name))?;
        if !self.dir.join(file.filename).exists() {
            self.fetch(model);
            return Err(anyhow!(
                "{} is not on disk; the models pipeline is downloading it",
                file.filename
            ));
        }
        let spec = EmbeddingModelSpec::resolve(model.name)?;
        // Read and loaded with no generation running: a chore making its context beside them would
        // leave the reading stale and stack the two allocations.
        let on_gpu = {
            let _setup = setup_for_load().await;
            let available = mem_available_mb();
            if embedder_on_gpu(
                pond_adapters_local_inference::CUDA_ENABLED,
                model.model_id,
                available,
            ) {
                let gpu = spec.clone().with_placement(Placement::Gpu);
                Some((available, self.loaded(gpu, file.filename).await))
            } else {
                None
            }
        };
        if let Some((available, gpu)) = on_gpu {
            let warmed = match gpu {
                Ok(provider) => Self::warmed(provider, file.filename).await,
                Err(e) => Err(e),
            };
            match warmed {
                Ok(provider) => return Ok(provider),
                // NvMap can refuse a load that MemAvailable said would fit; the CPU still serves.
                Err(e) => tracing::warn!(
                    model = model.name,
                    available_mb = ?available,
                    "the embedder did not load on the GPU, so it runs on the CPU: {e:#}"
                ),
            }
        }
        let provider = {
            let _setup = setup_for_load().await;
            self.loaded(spec, file.filename).await?
        };
        Self::warmed(provider, file.filename).await
    }

    /// Loads the model; the caller holds the GPU gate's setup side around it.
    #[cfg(feature = "local-inference")]
    async fn loaded(
        &self,
        spec: pond_inference::EmbeddingModelSpec,
        filename: &str,
    ) -> Result<pond_inference::GgufEmbeddingProvider> {
        use anyhow::Context as _;

        let provider = pond_inference::GgufEmbeddingProvider::new(spec, &self.dir);
        provider
            .load()
            .await
            .with_context(|| format!("loading {filename} on the {:?}", provider.placement()))?;
        Ok(provider)
    }

    /// One embed after the gate reopens, since a decode waits while it is shut: a placement that
    /// loads but cannot decode fails here, before the embedder is ready.
    #[cfg(feature = "local-inference")]
    async fn warmed(
        provider: pond_inference::GgufEmbeddingProvider,
        filename: &str,
    ) -> Result<Arc<dyn EmbeddingProvider>> {
        use anyhow::Context as _;

        provider.embed("warm up").await.with_context(|| {
            format!(
                "embedding with {filename} on the {:?}",
                provider.placement()
            )
        })?;
        tracing::info!(
            model_id = provider.model_id(),
            placement = ?provider.placement(),
            "GGUF embedder placed"
        );
        Ok(Arc::new(provider))
    }

    #[cfg(not(feature = "local-inference"))]
    async fn gguf(&self, model: &EmbeddingModel) -> Result<Arc<dyn EmbeddingProvider>> {
        Err(anyhow!(
            "{} runs on llama.cpp, which this build leaves out (the local-inference feature)",
            model.name
        ))
    }

    /// Starts the model's download through the acquisition pipeline, so it shows on Models.
    #[cfg_attr(not(feature = "local-inference"), allow(dead_code))]
    fn fetch(&self, model: &EmbeddingModel) {
        let Some(state) = self.state.get().and_then(Weak::upgrade) else {
            tracing::warn!(
                model = model.name,
                "the embedding model is missing and nothing can fetch it yet"
            );
            return;
        };
        let id = model.id();
        tokio::spawn(async move {
            pond_api::model_acquisition::restore_model(state, &id).await;
        });
    }
}

/// The GPU gate's setup side, for one embedder load; a wait for a generation is logged.
#[cfg(feature = "local-inference")]
async fn setup_for_load() -> pond_core::models::domain::gpu_gate::SetupGuard {
    let asked = std::time::Instant::now();
    let guard = pond_core::models::domain::gpu_gate::gpu_gate()
        .setup()
        .await;
    let waited = asked.elapsed();
    if waited >= Duration::from_millis(10) {
        tracing::info!(
            target: "giap::trace",
            kind = "embedder_load_waited",
            waited_ms = waited.as_millis() as u64,
            "the embedder's load waited for a generation to finish"
        );
    }
    guard
}

/// `MemAvailable` in MB; `None` off Linux or when unreadable.
#[cfg(feature = "local-inference")]
fn mem_available_mb() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb: u64 = info
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kb / 1024)
}

#[async_trait]
impl EmbedderFactory for ServerEmbedderFactory {
    async fn build(&self, model: &'static EmbeddingModel) -> Result<Arc<dyn EmbeddingProvider>> {
        match model.engine() {
            EmbeddingEngine::Fastembed => self.fastembed(model).await,
            EmbeddingEngine::LlamaCpp => self.gguf(model).await,
        }
    }
}
