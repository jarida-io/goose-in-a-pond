use anyhow::Result;
use async_trait::async_trait;

/// Result of dispatching a tool call to an MCP server.
#[derive(Debug, Clone)]
pub struct ToolCallResult {
    /// The text content returned by the tool (may include UI hints).
    pub content: String,
    pub success: bool,
}

/// Driven port: routes tool calls, by tool-name prefix, to the owning MCP server.
#[async_trait]
pub trait ToolDispatcher: Send + Sync {
    /// `tool_name` is the fully qualified name, e.g. "giap-weather__get_current_weather".
    async fn dispatch(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolCallResult>;

    async fn available_tools(&self) -> Vec<String>;

    /// `(name, description, parameters_json_schema)` for every available tool.
    async fn available_tool_definitions(&self) -> Vec<(String, String, serde_json::Value)> {
        self.available_tools()
            .await
            .into_iter()
            .map(|name| (name, String::new(), serde_json::json!({})))
            .collect()
    }

    /// OpenAI-compatible tools JSON, byte-identical to Goose's `format_tools()`, for
    /// `apply_chat_template_oaicompat()`; skips any conversion that could alter the schemas.
    async fn tools_json(&self) -> Option<String> {
        let defs = self.available_tool_definitions().await;
        if defs.is_empty() {
            return None;
        }
        let specs: Vec<serde_json::Value> = defs
            .into_iter()
            .map(|(name, desc, schema)| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": desc,
                        "parameters": schema,
                    }
                })
            })
            .collect();
        serde_json::to_string(&specs).ok()
    }

    /// Names and descriptions only: the fallback when full schemas exceed the token budget.
    async fn compact_tools_json(&self) -> Option<String> {
        let defs = self.available_tool_definitions().await;
        if defs.is_empty() {
            return None;
        }
        let specs: Vec<serde_json::Value> = defs
            .into_iter()
            .map(|(name, desc, _)| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": desc,
                    }
                })
            })
            .collect();
        serde_json::to_string(&specs).ok()
    }
}
