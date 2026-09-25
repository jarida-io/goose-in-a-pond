pub mod context;
pub mod providers;
pub mod voice;

// Services that stay at the `models/services/` root.
pub mod history_manager;
pub mod model_service;
pub mod prompt_builder;
pub mod thought_filter;

// Re-exports keeping the flat `models::services::<name>` paths resolving.
pub use context::{
    answer_contract, context_budget, context_governor, context_monitor, image_history, turn_budget,
};
pub use providers::fallback_provider;
pub use voice::{fallback_voice_output, instant_activation, spoken_time};
