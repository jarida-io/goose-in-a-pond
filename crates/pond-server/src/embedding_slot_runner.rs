//! Runs the embedding slot: keeps the chosen embedder loaded while the device has room for it, and
//! the index current for it, only while nothing the household waits on needs the device.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pond_core::context::embedding_slot::{EmbeddingSlot, PassEnd, SlotControl, SlotStores};
use pond_core::context::ports::ContextRepository;
use pond_core::context::vector_index::VectorIndex;
use pond_core::models::domain::{device_budget, embedding_models};
use pond_core::models::ports::embedding::{EmbedderFactory, EmbeddingProvider};
use pond_core::models::services::active_embedder::{ActiveEmbedder, EmbedderState};
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use tokio::sync::{Notify, RwLock};

use crate::inference_lane_runner::InferenceLane;

/// How often the slot looks for work no bell announced; a choice, not a measurement.
const POLL: Duration = Duration::from_secs(60);

/// How soon it looks again after the household held it off with work left; a choice.
const RETRY: Duration = Duration::from_secs(5);

/// Quiet period after a person's turn before a model with no room to stay is loaded for a pass;
/// a hand-asked reindex waives it.
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
    /// Rung by writers of rows that need a vector, and by work that loaded the embedder itself.
    pub arrived: Arc<Notify>,
    /// Rung when a person starts a turn, the moment memory is likeliest to run short.
    pub turn_started: Arc<Notify>,
    /// True while the chat model loads and prefills, which a model load beside it would slow.
    pub chat_model_warming: Box<dyn Fn() -> bool + Send + Sync>,
    /// The host's MemAvailable (MB); `None` where it cannot be read.
    pub available_mb: Box<dyn Fn() -> Option<u64> + Send + Sync>,
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
        "embedding slot active: keeps the Memory-role model loaded while there is room, \
         otherwise loads it for each pass and unloads after"
    );
    loop {
        let wait = if last == Look::HeldOff { RETRY } else { POLL };
        tokio::select! {
            _ = deps.reindex.notified() => {
                slot.retry_failed();
                user_requested = true;
            }
            _ = deps.arrived.notified() => {}
            _ = deps.turn_started.notified() => {}
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
        let stays = release_unless_room(deps).await;
        if !deps.lane.embedding_may_run().await || (deps.chat_model_warming)() {
            return Look::HeldOff;
        }

        let stores = SlotStores {
            index: deps.index.as_ref(),
            memories: deps.memories.as_ref(),
            sessions: deps.sessions.as_ref(),
            context: deps.context.as_deref(),
        };
        // Without room to stay, the model loads only for a backlog, and only once the house is quiet.
        if !stays {
            if !*user_requested && deps.activity.read().await.elapsed() < IDLE_BEFORE_LOAD {
                return Look::HeldOff;
            }
            if !slot.has_pending_work(&stores, &deps.embedder).await {
                *load_owed = false;
                *user_requested = false;
                return Look::Done;
            }
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
        let end = match report.end {
            // The new choice loads here, at once, while the device is still free.
            PassEnd::ModelChanged => {
                *load_owed = true;
                continue;
            }
            PassEnd::Yielded => Look::HeldOff,
            PassEnd::Drained | PassEnd::NotReady | PassEnd::StoreFailed => {
                *user_requested = false;
                Look::Done
            }
        };
        release_unless_room(deps).await;
        return end;
    }
}

/// Unloads the Memory model when the device has no room for it to stay, so turns recall by keyword
/// until it is back; returns whether it may stay.
async fn release_unless_room(deps: &SlotDeps) -> bool {
    let available_mb = (deps.available_mb)();
    let estimate_mb = embedding_models::find_by_model_id(&deps.embedder.model_id())
        .map_or(0, |m| m.ram_estimate_mb());
    if device_budget::embedder_may_stay(available_mb, deps.embedder.available(), estimate_mb) {
        return true;
    }
    if deps.embedder.unload().await {
        tracing::info!(
            available_mb = ?available_mb,
            margin_mb = device_budget::EMBEDDER_MARGIN_MB,
            "memory is short, so the Memory model is unloaded until a pass needs it"
        );
    }
    false
}
