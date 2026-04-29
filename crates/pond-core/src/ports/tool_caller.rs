//! ToolCaller port — driven port for generating tool-call arguments via a specialist model.

use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: tool-call argument generation.
///
/// When the main LLM decides to call an MCP tool but fails to produce valid
/// arguments (common with small local GGUF models), this specialist model
/// generates the structured arguments from the tool schema + user query.
///
/// The specialist sees NO conversation history — only the tool schema and the
/// user's original request. This keeps inference fast and context-free.
#[async_trait]
pub trait ToolCaller: Send + Sync {
    /// Generate tool-call arguments for the given tool and user query.
    ///
    /// Returns the arguments as a JSON map (e.g. `{"topic": "Kenya"}`).
    /// The caller is responsible for passing these to the MCP tool.
    async fn generate_tool_call(
        &self,
        tool_name: &str,
        tool_schema_json: &str,
        user_query: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>>;
}
