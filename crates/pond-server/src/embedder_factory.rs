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
        use anyhow::Context as _;

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
        let spec = pond_inference::EmbeddingModelSpec::resolve(model.name)?;
        let provider = pond_inference::GgufEmbeddingProvider::new(spec, &self.dir);
        // The provider loads on its first embed; doing that here keeps the load out of a turn.
        provider
            .embed("warm up")
            .await
            .with_context(|| format!("loading {}", file.filename))?;
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

#[async_trait]
impl EmbedderFactory for ServerEmbedderFactory {
    async fn build(&self, model: &'static EmbeddingModel) -> Result<Arc<dyn EmbeddingProvider>> {
        match model.engine() {
            EmbeddingEngine::Fastembed => self.fastembed(model).await,
            EmbeddingEngine::LlamaCpp => self.gguf(model).await,
        }
    }
}
