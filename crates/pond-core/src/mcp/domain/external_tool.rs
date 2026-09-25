//! External tool descriptions for the dynamic tool registry.

/// A tool available to the agent; built-in and MCP-extension tools share it to render together.
#[derive(Debug, Clone)]
pub struct ExternalToolDescription {
    /// Tool name as it appears in the MCP schema (e.g. "wikipedia", "get_weather").
    pub tool_name: String,
    /// Name of the extension that provides this tool (e.g. "giap", "filesystem-server").
    pub extension_name: String,
    pub description: String,
    /// True for GIAP's built-in tools, false for user-added MCP extensions.
    pub is_builtin: bool,
}
