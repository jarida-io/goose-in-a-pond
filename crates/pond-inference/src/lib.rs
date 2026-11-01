//! `pond-inference` -- GIAP's independent llama.cpp inference engine.
//!
//! Wraps [`llama-cpp-2`] directly for in-process GGUF inference without any
//! Goose dependency. Implements the [`InferenceProvider`] port trait from
//! `pond-core` so the agent loop can call `stream_chat()` with tool
//! definitions and receive streaming [`ChatEvent`]s.
//!
//! # Architecture
//!
//! ```text
//! LlamaCppEngine (owns LlamaBackend + loaded model)
//!   |
//!   +-- InferenceProvider::stream_chat()
//!   |     |-- build prompt via chat template (jinja, native tools)
//!   |     |-- tokenize
//!   |     |-- spawn_blocking: create context, prefill, generation loop
//!   |     |-- mpsc channel -> ChatEventStream
//!   |     +-- parse tool calls from generated text
//!   |
//!   +-- load_model() / unload_model()
//!   +-- estimate_max_context()
//! ```
//!
//! # Hardware acceleration
//! - **macOS**: Metal activated automatically via `llama-cpp-2` cfg flags.
//! - **Jetson Orin Nano (NVIDIA)**: Requires `--features cuda` at build time.
//!
//! # Example
//! ```no_run
//! # async fn example() -> anyhow::Result<()> {
//! use pond_inference::LlamaCppEngine;
//! use std::path::Path;
//!
//! let engine = LlamaCppEngine::new(Path::new("/data/models"))?;
//! engine.load_model("gemma-4-E2B-it-Q4_K_M.gguf", 99, true).await?;
//! // engine now implements InferenceProvider -- pass it to the agent loop
//! # Ok(())
//! # }
//! ```

pub mod embedding;
mod engine;
pub mod kv_cache;
mod memory;
mod provider;
mod sampling;
mod tool_calling;

pub use embedding::{ContextPlan, EmbeddingModelSpec, GgufEmbeddingProvider, Placement};
pub use engine::LlamaCppEngine;
