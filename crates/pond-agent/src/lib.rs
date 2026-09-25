//! GIAP's own [`Agent`](pond_core::models::ports::agent::Agent) loop over an Ollama provider.

pub mod agent;
pub mod history;
pub mod ollama_provider;
pub mod ollama_wire;
pub mod tool_bridge;

pub use agent::PondAgent;
pub use ollama_provider::OllamaInferenceProvider;
