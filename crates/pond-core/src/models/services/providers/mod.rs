//! Provider composition. Keyword pre-classification is ruled out (AGENTS.md); per-role
//! dispatch belongs at the `pond-api` `AppState` seam, beside the provider hot-swap.
pub mod fallback_provider;
