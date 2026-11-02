//! The one embedder the pond runs, switchable without a restart.
//! Only one model is ever resident: a switch drops the old one before the new one is built.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tokio::sync::{Notify, RwLock, RwLockReadGuard};

use crate::models::domain::embedding_models::{EmbedderChoice, EmbeddingModel};
use crate::models::ports::embedding::{EmbedderFactory, EmbeddingProvider};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedderState {
    /// Semantic search is switched off.
    Off,
    /// Chosen, and waiting for background work to load it.
    Chosen {
        model_id: String,
    },
    Loading {
        model_id: String,
    },
    Ready {
        model_id: String,
        dims: usize,
    },
    Failed {
        model_id: String,
        reason: String,
    },
}

impl EmbedderState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Chosen { .. } => "chosen",
            Self::Loading { .. } => "loading",
            Self::Ready { .. } => "ready",
            Self::Failed { .. } => "failed",
        }
    }

    pub fn model_id(&self) -> Option<&str> {
        match self {
            Self::Off => None,
            Self::Chosen { model_id }
            | Self::Loading { model_id }
            | Self::Ready { model_id, .. }
            | Self::Failed { model_id, .. } => Some(model_id),
        }
    }
}

impl fmt::Display for EmbedderState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.model_id() {
            Some(model_id) => write!(f, "{} {model_id}", self.as_str()),
            None => f.write_str(self.as_str()),
        }
    }
}

struct Status {
    /// `None` for a provider handed in already built.
    choice: Option<EmbedderChoice>,
    state: EmbedderState,
    generation: u64,
}

impl Status {
    fn chosen_model(&self) -> Option<&'static EmbeddingModel> {
        self.choice.and_then(|c| c.model())
    }
}

/// An [`EmbeddingProvider`] over whichever model the household chose.
pub struct ActiveEmbedder {
    /// `Some` exactly while the state is `Ready`; both change only under its write lock, so a
    /// read guard pins the model a vector is made with.
    inner: RwLock<Option<Arc<dyn EmbeddingProvider>>>,
    status: Mutex<Status>,
    load_wanted: Notify,
    /// One build at a time, so two models never load side by side.
    loading: tokio::sync::Mutex<()>,
}

impl Default for ActiveEmbedder {
    fn default() -> Self {
        Self::off()
    }
}

impl ActiveEmbedder {
    pub fn off() -> Self {
        Self {
            inner: RwLock::new(None),
            status: Mutex::new(Status {
                choice: Some(EmbedderChoice::Off),
                state: EmbedderState::Off,
                generation: 0,
            }),
            load_wanted: Notify::new(),
            loading: tokio::sync::Mutex::new(()),
        }
    }

    /// Ready with a provider built elsewhere; a later [`Self::choose`] replaces it.
    pub fn loaded(provider: Arc<dyn EmbeddingProvider>) -> Self {
        let state = EmbedderState::Ready {
            model_id: provider.model_id(),
            dims: provider.dimensions(),
        };
        Self {
            inner: RwLock::new(Some(provider)),
            status: Mutex::new(Status {
                choice: None,
                state,
                generation: 0,
            }),
            load_wanted: Notify::new(),
            loading: tokio::sync::Mutex::new(()),
        }
    }

    // A poisoned lock still holds a consistent status: every write to it is a plain assignment.
    fn status(&self) -> MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> EmbedderState {
        self.status().state.clone()
    }

    /// Records a new choice, dropping the loaded model first. It does not load the new one.
    /// Returns whether the choice changed.
    pub async fn choose(&self, choice: EmbedderChoice) -> bool {
        if self.already_chosen(choice) {
            return false;
        }
        // Waits for every embed and lease in flight, so nothing still reads the old model.
        let mut inner = self.inner.write().await;
        if self.already_chosen(choice) {
            return false;
        }
        drop(inner.take());
        {
            let mut status = self.status();
            status.choice = Some(choice);
            status.state = match choice.model() {
                None => EmbedderState::Off,
                Some(model) => EmbedderState::Chosen {
                    model_id: model.model_id.to_string(),
                },
            };
            status.generation += 1;
            tracing::info!(state = %status.state, generation = status.generation, "embedder chosen");
        }
        drop(inner);
        if choice.model().is_some() {
            self.load_wanted.notify_one();
        }
        true
    }

    /// True when `choice` is current; a failed load of it is asked for again.
    fn already_chosen(&self, choice: EmbedderChoice) -> bool {
        let status = self.status();
        if status.choice != Some(choice) {
            return false;
        }
        if matches!(status.state, EmbedderState::Failed { .. }) {
            self.load_wanted.notify_one();
        }
        true
    }

    /// Asks background work to load the chosen model, e.g. once its missing file has arrived.
    pub fn request_load(&self) {
        let status = self.status();
        if status.chosen_model().is_some() && !matches!(status.state, EmbedderState::Ready { .. }) {
            self.load_wanted.notify_one();
        }
    }

    /// Resolves when a load has been asked for.
    pub async fn load_requested(&self) {
        self.load_wanted.notified().await;
    }

    /// Builds the chosen model. Only background work calls this, so a load never lands in a turn.
    pub async fn ensure_loaded(&self, factory: &dyn EmbedderFactory) -> EmbedderState {
        let _one_build = self.loading.lock().await;
        let (model, generation) = {
            let mut status = self.status();
            let Some(model) = status.chosen_model() else {
                return status.state.clone();
            };
            if matches!(status.state, EmbedderState::Ready { .. }) {
                return status.state.clone();
            }
            status.state = EmbedderState::Loading {
                model_id: model.model_id.to_string(),
            };
            (model, status.generation)
        };

        let built = factory
            .build(model)
            .await
            .and_then(|provider| matches_row(model, provider));

        let mut inner = self.inner.write().await;
        let mut status = self.status();
        if status.generation != generation {
            // Chosen away while it built; the newer choice asks for its own load.
            let state = status.state.clone();
            drop(status);
            drop(inner);
            drop(built);
            return state;
        }
        status.state = match built {
            Ok(provider) => {
                let dims = provider.dimensions();
                *inner = Some(provider);
                tracing::info!(model_id = model.model_id, dims, "embedding model ready");
                EmbedderState::Ready {
                    model_id: model.model_id.to_string(),
                    dims,
                }
            }
            Err(e) => {
                tracing::warn!(
                    model_id = model.model_id,
                    "embedding model did not load: {e:#}"
                );
                EmbedderState::Failed {
                    model_id: model.model_id.to_string(),
                    reason: format!("{e:#}"),
                }
            }
        };
        status.state.clone()
    }

    /// The loaded model, held until the lease drops: a switch waits for it.
    pub async fn lease(&self) -> Option<EmbedLease<'_>> {
        let provider = RwLockReadGuard::try_map(self.inner.read().await, Option::as_ref).ok()?;
        Some(EmbedLease { provider })
    }

    fn not_ready(&self) -> anyhow::Error {
        anyhow!("no embedding model is ready ({})", self.state())
    }
}

/// A provider that disagrees with its table row would stamp its vectors with the wrong space.
fn matches_row(
    model: &EmbeddingModel,
    provider: Arc<dyn EmbeddingProvider>,
) -> Result<Arc<dyn EmbeddingProvider>> {
    let (id, dims) = (provider.model_id(), provider.dimensions());
    if id != model.model_id || dims != model.dims {
        return Err(anyhow!(
            "the loaded embedder reports {id} at {dims} dimensions, but {} is {} wide",
            model.model_id,
            model.dims
        ));
    }
    Ok(provider)
}

/// Embeds with one model and names it, so a vector and its model stamp always match.
pub struct EmbedLease<'a> {
    provider: RwLockReadGuard<'a, Arc<dyn EmbeddingProvider>>,
}

impl EmbedLease<'_> {
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.provider.embed(text).await
    }

    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.provider.embed_query(text).await
    }

    pub fn model_id(&self) -> String {
        self.provider.model_id()
    }

    pub fn dims(&self) -> usize {
        self.provider.dimensions()
    }
}

#[async_trait]
impl EmbeddingProvider for ActiveEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let lease = self.lease().await.ok_or_else(|| self.not_ready())?;
        lease.embed(text).await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        // A turn never waits on a switch: the caller falls back to keywords instead.
        let guard = self
            .inner
            .try_read()
            .map_err(|_| anyhow!("the embedding model is being switched"))?;
        let provider = guard.as_ref().ok_or_else(|| self.not_ready())?;
        provider.embed_query(text).await
    }

    /// The loaded width, or the chosen model's while it loads; 0 when off.
    fn dimensions(&self) -> usize {
        let status = self.status();
        match &status.state {
            EmbedderState::Ready { dims, .. } => *dims,
            _ => status.chosen_model().map_or(0, |m| m.dims),
        }
    }

    fn model_id(&self) -> String {
        self.status()
            .state
            .model_id()
            .unwrap_or_default()
            .to_string()
    }

    fn available(&self) -> bool {
        matches!(self.status().state, EmbedderState::Ready { .. })
    }

    fn generation(&self) -> u64 {
        self.status().generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::embedding_models::{
        ALL_MINILM_L6_V2, BGE_SMALL_EN_V1_5, NOMIC_EMBED_TEXT_V1_5,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    const MINILM: EmbedderChoice = EmbedderChoice::Model(&ALL_MINILM_L6_V2);
    const BGE_SMALL: EmbedderChoice = EmbedderChoice::Model(&BGE_SMALL_EN_V1_5);
    const NOMIC: EmbedderChoice = EmbedderChoice::Model(&NOMIC_EMBED_TEXT_V1_5);

    struct Counted {
        model: &'static EmbeddingModel,
        live: Arc<AtomicUsize>,
    }

    impl Drop for Counted {
        fn drop(&mut self) {
            self.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl EmbeddingProvider for Counted {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![1.0; self.model.dims])
        }
        fn dimensions(&self) -> usize {
            self.model.dims
        }
        fn model_id(&self) -> String {
            self.model.model_id.to_string()
        }
    }

    /// Counts live providers, and records how many were live when each build began.
    #[derive(Default)]
    struct CountingFactory {
        live: Arc<AtomicUsize>,
        builds: AtomicUsize,
        live_at_build: Mutex<Vec<usize>>,
        fail: std::sync::atomic::AtomicBool,
        delay: Option<Duration>,
    }

    #[async_trait]
    impl EmbedderFactory for CountingFactory {
        async fn build(
            &self,
            model: &'static EmbeddingModel,
        ) -> Result<Arc<dyn EmbeddingProvider>> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            self.live_at_build
                .lock()
                .unwrap()
                .push(self.live.load(Ordering::SeqCst));
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err(anyhow!("file not on disk"));
            }
            self.live.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(Counted {
                model,
                live: self.live.clone(),
            }))
        }
    }

    async fn ready(choice: EmbedderChoice, factory: &CountingFactory) -> ActiveEmbedder {
        let embedder = ActiveEmbedder::off();
        assert!(embedder.choose(choice).await);
        assert!(matches!(
            embedder.ensure_loaded(factory).await,
            EmbedderState::Ready { .. }
        ));
        embedder
    }

    #[tokio::test]
    async fn the_old_model_is_dropped_before_the_new_one_is_built() {
        let factory = CountingFactory::default();
        let embedder = ready(MINILM, &factory).await;
        assert_eq!(factory.live.load(Ordering::SeqCst), 1);

        assert!(embedder.choose(NOMIC).await);
        assert_eq!(
            factory.live.load(Ordering::SeqCst),
            0,
            "choose drops the old model"
        );
        assert_eq!(
            embedder.state(),
            EmbedderState::Chosen {
                model_id: "nomic-embed-text-v1.5".into()
            }
        );

        let state = embedder.ensure_loaded(&factory).await;
        assert_eq!(
            state,
            EmbedderState::Ready {
                model_id: "nomic-embed-text-v1.5".into(),
                dims: 768
            }
        );
        assert_eq!(*factory.live_at_build.lock().unwrap(), vec![0, 0]);
        assert_eq!(factory.live.load(Ordering::SeqCst), 1);
        assert_eq!(embedder.dimensions(), 768);
    }

    #[tokio::test]
    async fn a_held_lease_blocks_a_switch_until_it_is_released() {
        let factory = CountingFactory::default();
        let embedder = Arc::new(ready(MINILM, &factory).await);

        let lease = embedder.lease().await.expect("ready");
        let switching = {
            let embedder = embedder.clone();
            tokio::spawn(async move { embedder.choose(BGE_SMALL).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!switching.is_finished(), "the switch waits for the lease");
        assert_eq!(lease.model_id(), "all-MiniLM-L6-v2");
        assert_eq!(lease.embed("x").await.unwrap().len(), 384);

        drop(lease);
        assert!(switching.await.unwrap());
        assert_eq!(factory.live.load(Ordering::SeqCst), 0);
        assert!(
            embedder.lease().await.is_none(),
            "nothing is loaded until background work loads it"
        );
    }

    #[tokio::test]
    async fn a_query_fails_fast_while_a_switch_waits() {
        let factory = CountingFactory::default();
        let embedder = Arc::new(ready(MINILM, &factory).await);

        let lease = embedder.lease().await.expect("ready");
        let switching = {
            let embedder = embedder.clone();
            tokio::spawn(async move { embedder.choose(BGE_SMALL).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;

        let query = tokio::time::timeout(Duration::from_millis(50), embedder.embed_query("hi"))
            .await
            .expect("a query never waits on a switch");
        assert!(query.is_err());

        drop(lease);
        switching.await.unwrap();
        assert!(
            embedder.embed_query("hi").await.is_err(),
            "chosen but not loaded"
        );
    }

    #[tokio::test]
    async fn embedding_never_loads_a_model() {
        let factory = CountingFactory::default();
        let embedder = ActiveEmbedder::off();
        embedder.choose(NOMIC).await;

        assert!(embedder.embed_query("hi").await.is_err());
        assert!(embedder.embed("hi").await.is_err());
        assert!(!embedder.available());
        assert_eq!(factory.builds.load(Ordering::SeqCst), 0);
        assert_eq!(embedder.model_id(), "nomic-embed-text-v1.5");
        assert_eq!(embedder.dimensions(), 768);
    }

    #[tokio::test]
    async fn choosing_the_current_model_again_keeps_it_loaded() {
        let factory = CountingFactory::default();
        let embedder = ready(MINILM, &factory).await;
        let generation = embedder.generation();

        assert!(!embedder.choose(MINILM).await);
        assert!(embedder.available());
        assert_eq!(embedder.generation(), generation);
        assert_eq!(factory.builds.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn the_generation_moves_on_a_switch_and_not_on_a_load() {
        let factory = CountingFactory::default();
        let embedder = ActiveEmbedder::off();
        embedder.choose(MINILM).await;
        let chosen = embedder.generation();
        embedder.ensure_loaded(&factory).await;
        assert_eq!(embedder.generation(), chosen);
        embedder.choose(BGE_SMALL).await;
        assert!(embedder.generation() > chosen);
    }

    #[tokio::test]
    async fn a_failed_load_reports_why_and_retries_when_asked_again() {
        let factory = CountingFactory::default();
        factory.fail.store(true, Ordering::SeqCst);
        let embedder = ActiveEmbedder::off();
        embedder.choose(NOMIC).await;
        let state = embedder.ensure_loaded(&factory).await;
        assert!(
            matches!(&state, EmbedderState::Failed { reason, .. } if reason.contains("not on disk")),
            "{state:?}"
        );
        assert!(!embedder.available());

        factory.fail.store(false, Ordering::SeqCst);
        // Choosing it again asks for another load rather than reporting a change.
        assert!(!embedder.choose(NOMIC).await);
        assert!(matches!(
            embedder.ensure_loaded(&factory).await,
            EmbedderState::Ready { .. }
        ));
    }

    #[tokio::test]
    async fn a_build_overtaken_by_a_new_choice_is_never_installed() {
        let factory = Arc::new(CountingFactory {
            delay: Some(Duration::from_millis(50)),
            ..Default::default()
        });
        let embedder = Arc::new(ActiveEmbedder::off());
        embedder.choose(NOMIC).await;
        let loading = {
            let (embedder, factory) = (embedder.clone(), factory.clone());
            tokio::spawn(async move { embedder.ensure_loaded(factory.as_ref()).await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(matches!(embedder.state(), EmbedderState::Loading { .. }));
        embedder.choose(MINILM).await;

        loading.await.unwrap();
        assert!(!embedder.available());
        assert_eq!(
            factory.live.load(Ordering::SeqCst),
            0,
            "the stale build is dropped"
        );
        assert_eq!(
            embedder.ensure_loaded(factory.as_ref()).await,
            EmbedderState::Ready {
                model_id: "all-MiniLM-L6-v2".into(),
                dims: 384
            }
        );
    }

    #[tokio::test]
    async fn a_provider_that_disagrees_with_its_row_is_refused() {
        struct Liar;
        #[async_trait]
        impl EmbedderFactory for Liar {
            async fn build(
                &self,
                _: &'static EmbeddingModel,
            ) -> Result<Arc<dyn EmbeddingProvider>> {
                Ok(Arc::new(Counted {
                    model: &BGE_SMALL_EN_V1_5,
                    live: Arc::new(AtomicUsize::new(1)),
                }))
            }
        }
        let embedder = ActiveEmbedder::off();
        embedder.choose(MINILM).await;
        assert!(matches!(
            embedder.ensure_loaded(&Liar).await,
            EmbedderState::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn off_loads_nothing_and_reports_no_model() {
        let factory = CountingFactory::default();
        let embedder = ready(MINILM, &factory).await;
        assert!(embedder.choose(EmbedderChoice::Off).await);
        assert_eq!(embedder.ensure_loaded(&factory).await, EmbedderState::Off);
        assert_eq!(factory.live.load(Ordering::SeqCst), 0);
        assert_eq!(embedder.model_id(), "");
        assert_eq!(embedder.dimensions(), 0);
        assert!(!embedder.available());
    }

    #[tokio::test]
    async fn a_choice_asks_for_a_load() {
        let embedder = ActiveEmbedder::off();
        embedder.choose(MINILM).await;
        tokio::time::timeout(Duration::from_millis(50), embedder.load_requested())
            .await
            .expect("choose rings the loader");
    }
}
