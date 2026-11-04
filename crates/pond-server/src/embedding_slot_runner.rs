//! Runs the embedding slot: loads the chosen embedder and keeps the index current for it, only
//! while nothing the household waits on needs the device.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pond_core::context::embedding_slot::{EmbeddingSlot, PassEnd, SlotControl, SlotStores};
use pond_core::context::ports::ContextRepository;
use pond_core::context::vector_index::VectorIndex;
use pond_core::models::ports::embedding::EmbedderFactory;
use pond_core::models::services::active_embedder::{ActiveEmbedder, EmbedderState};
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use tokio::sync::{Notify, RwLock};

use crate::inference_lane_runner::InferenceLane;

/// How often the slot looks for work no bell announced; a choice, not a measurement.
const POLL: Duration = Duration::from_secs(60);

/// How soon it looks again after the household held it off with work left; a choice.
const RETRY: Duration = Duration::from_secs(5);

/// Quiet period after a person's turn before the embedder is loaded on its own; a hand-asked
/// reindex waives it.
const IDLE_BEFORE_LOAD: Duration = Duration::from_secs(120);

pub struct SlotDeps {
    pub embedder: Arc<ActiveEmbedder>,
    pub factory: Arc<dyn EmbedderFactory>,
    pub lane: Arc<InferenceLane>,
    pub index: Arc<dyn VectorIndex>,
    pub memories: Arc<dyn MemoryRepository>,
    pub sessions: Arc<dyn SessionStorage>,
    pub context: Option<Arc<dyn ContextRepository>>,
    /// Stamped when a person starts a turn.
    pub activity: Arc<RwLock<Instant>>,
    /// Rung by the Reindex route: rows that failed are tried again.
    pub reindex: Arc<Notify>,
    /// Rung by writers of rows that need a vector.
    pub arrived: Arc<Notify>,
    /// True while the chat model loads and prefills, which a model load beside it would slow.
    pub chat_model_warming: Box<dyn Fn() -> bool + Send + Sync>,
}

/// Yields once the lane refuses, or a person has started something since the pass began.
struct Gate {
    lane: Arc<InferenceLane>,
    activity: Arc<RwLock<Instant>>,
    since: Instant,
}

#[async_trait]
impl SlotControl for Gate {
    async fn may_continue(&self) -> bool {
        *self.activity.read().await <= self.since && self.lane.embedding_may_run().await
    }
}

/// What the last look found, which sets how soon the next one comes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Done,
    HeldOff,
}

pub async fn run(deps: SlotDeps) {
    let mut slot = EmbeddingSlot::new();
    let mut load_owed = false;
    let mut user_requested = false;
    let mut last = Look::Done;
    tracing::info!(
        "embedding slot active: loads the Memory-role model on demand while idle and unloads after"
    );
    loop {
        let wait = if last == Look::HeldOff { RETRY } else { POLL };
        tokio::select! {
            _ = deps.reindex.notified() => {
                slot.retry_failed();
                user_requested = true;
            }
            _ = deps.arrived.notified() => {}
            _ = deps.embedder.load_requested() => load_owed = true,
            _ = tokio::time::sleep(wait) => {}
        }
        last = look(&deps, &mut slot, &mut load_owed, &mut user_requested).await;
    }
}

async fn look(
    deps: &SlotDeps,
    slot: &mut EmbeddingSlot,
    load_owed: &mut bool,
    user_requested: &mut bool,
) -> Look {
    loop {
        if !deps.lane.embedding_may_run().await || (deps.chat_model_warming)() {
            deps.embedder.unload().await;
            return Look::HeldOff;
        }
        if !*user_requested && deps.activity.read().await.elapsed() < IDLE_BEFORE_LOAD {
            deps.embedder.unload().await;
            return Look::HeldOff;
        }

        let stores = SlotStores {
            index: deps.index.as_ref(),
            memories: deps.memories.as_ref(),
            sessions: deps.sessions.as_ref(),
            context: deps.context.as_deref(),
        };
        if !slot.has_pending_work(&stores, &deps.embedder).await {
            *load_owed = false;
            *user_requested = false;
            deps.embedder.unload().await;
            return Look::Done;
        }

        if *load_owed || matches!(deps.embedder.state(), EmbedderState::Chosen { .. }) {
            *load_owed = false;
            let _shown = deps.lane.embedding_slot_running();
            if !matches!(
                deps.embedder.ensure_loaded(deps.factory.as_ref()).await,
                EmbedderState::Ready { .. }
            ) {
                *user_requested = false;
                return Look::Done;
            }
        }

        let gate = Gate {
            lane: deps.lane.clone(),
            activity: deps.activity.clone(),
            since: *deps.activity.read().await,
        };
        let report = {
            let _shown = deps.lane.embedding_slot_running();
            slot.run_pass(&stores, &deps.embedder, &gate).await
        };
        match report.end {
            // The new choice loads here, at once, while the device is still free.
            PassEnd::ModelChanged => *load_owed = true,
            PassEnd::Yielded => {
                deps.embedder.unload().await;
                return Look::HeldOff;
            }
            PassEnd::Drained | PassEnd::NotReady | PassEnd::StoreFailed => {
                *user_requested = false;
                deps.embedder.unload().await;
                return Look::Done;
            }
        }
    }
}
