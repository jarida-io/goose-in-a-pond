//! The pond's chat model as an inline step calls it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use pond_core::models::domain::inference_bus::{
    inference_bus, running_under, InferenceClaim, InferenceHeldFloor, InferenceHold,
};
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;

use super::{EngineModel, ModelStop};

/// Whichever provider the pond's chat runs on now, called on the inference bus's Task lane.
pub struct ProviderModel {
    provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
}

impl ProviderModel {
    pub fn new(provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl EngineModel for ProviderModel {
    async fn complete(&self, system: &str, user: String) -> Result<String, ModelStop> {
        let Some(provider) = self.provider.read().await.clone() else {
            return Err(ModelStop::Failed("no model is chosen".to_string()));
        };
        let held = InferenceHeldFloor::new(inference_bus(), InferenceClaim::Task);
        let give_way = tokio::spawn({
            let held = held.clone();
            async move { held.give_way().await }
        });
        // A cut call comes back as a plain error, so a turn seen on the bus is what tells it
        // from a call that failed.
        let turn_came = Arc::new(AtomicBool::new(false));
        let watch = tokio::spawn({
            let turn_came = turn_came.clone();
            let mut states = inference_bus().subscribe();
            async move {
                loop {
                    if states
                        .borrow_and_update()
                        .has(|claim| claim == InferenceClaim::Turn)
                    {
                        turn_came.store(true, Ordering::Relaxed);
                        return;
                    }
                    if states.changed().await.is_err() {
                        return;
                    }
                }
            }
        });
        let reply = running_under(
            InferenceHold::Held(held),
            provider.complete(system, vec![ChatMessage::user(user)]),
        )
        .await;
        give_way.abort();
        watch.abort();
        match reply {
            Ok(message) => Ok(message.content),
            Err(_) if turn_came.load(Ordering::Relaxed) => Err(ModelStop::Cut),
            Err(e) => Err(ModelStop::Failed(e.to_string())),
        }
    }
}
