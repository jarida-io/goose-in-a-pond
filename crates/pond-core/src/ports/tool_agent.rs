//! ToolAgent port — driven port for the pre-inference tool agent.
//!
//! The Tool Agent classifies a user message, optionally fetches external
//! data (Wikipedia, weather, memory, etc.), and returns augmented context
//! to inject into the agent's message before the main LLM runs.

use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: pre-inference tool agent.
///
/// Runs BEFORE the main LLM. Classifies the user's message, executes
/// any needed tool (Wikipedia lookup, weather fetch, memory save/recall),
/// and returns an augmented message with the tool result injected.
///
/// Returns `Ok(None)` if no tool was needed — the original message
/// should be used as-is.
#[async_trait]
pub trait ToolAgent: Send + Sync {
    /// Classify and optionally execute a tool for the given message.
    ///
    /// Returns `Some(augmented_message)` if a tool was used (the message
    /// includes the original text + retrieved information + instructions).
    /// Returns `None` if no tool was needed.
    async fn process(&self, message: &str) -> Result<Option<String>>;
}
