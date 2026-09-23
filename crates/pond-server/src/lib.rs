//! Library target for `pond-server`.
//!
//! Exposing key modules here allows integration tests in `tests/` to import
//! them without duplicating code from `main.rs`.

pub mod account_sync;
pub mod hf_cache_migration;
pub mod llm_memory_consolidator;
pub mod llm_memory_extractor;
pub mod schedule_executors;
pub mod startup;

#[cfg(unix)]
pub mod embedded_network;
pub mod tls_identity;
