use anyhow::Result;
use async_trait::async_trait;

/// Specialist that regenerates invalid tool-call arguments from schema and request, no history.
#[async_trait]
pub trait ToolCaller: Send + Sync {
    /// Only generates the arguments; the caller passes them to the MCP tool.
    async fn generate_tool_call(
        &self,
        tool_name: &str,
        tool_schema_json: &str,
        user_query: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>>;
}
