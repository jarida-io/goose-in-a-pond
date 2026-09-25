//! Context window: [`context_governor`] resolves it, [`context_budget`] splits it,
//! [`context_monitor`] tracks use, [`model_class`] picks the compaction a model can afford.
pub mod answer_contract;
pub mod context_budget;
pub mod context_governor;
pub mod context_monitor;
pub mod image_history;
pub mod model_class;
pub mod prefix_cache;
pub mod token_counting;
pub mod turn_budget;
pub mod turn_trimmer;
