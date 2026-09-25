use async_trait::async_trait;

use crate::mcp::domain::external_tool::ExternalToolDescription;

/// All tool descriptions: built-ins seeded at construction, extension tools added/removed live.
#[async_trait]
pub trait ToolRegistryPort: Send + Sync {
    async fn all_tools(&self) -> Vec<ExternalToolDescription>;

    /// System-prompt lines, `"tool_name -- description"` (external: `"extension/tool_name -- …"`).
    /// `compact` truncates descriptions to 80 characters.
    async fn prompt_description_lines(&self, compact: bool) -> Vec<String>;

    /// Replaces any tools previously registered for `ext_name`; `tools` is `(name, description)`.
    async fn register_extension_tools(&self, ext_name: &str, tools: Vec<(String, String)>);

    async fn deregister_extension(&self, ext_name: &str);

    /// The extension that provides `tool_name`.
    async fn resolve_extension(&self, tool_name: &str) -> Option<String>;
}
