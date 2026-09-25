use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionInfo {
    pub name: String,
    pub kind: String, // "builtin" | "stdio" | "http"
    pub description: String,
    pub tools: Vec<String>,
    /// Disabled extensions are not loaded into Goose agent sessions.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Extension connection status: "connected", "error", or "loading"
    #[serde(default = "default_status")]
    pub status: String,
    /// Last error message if status is "error"
    #[serde(default)]
    pub last_error: Option<String>,
}

fn default_enabled() -> bool {
    true
}
fn default_status() -> String {
    "connected".to_string()
}

/// An MCP tool with its owning extension and description, for the Extensions > Tools UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub extension: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddExtensionRequest {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub description: String,
    // For stdio extensions:
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    // For http extensions:
    pub uri: Option<String>,
}

#[async_trait]
pub trait ExtensionManagerPort: Send + Sync {
    async fn list_extensions(&self) -> Result<Vec<ExtensionInfo>>;
    async fn add_extension(&self, request: AddExtensionRequest) -> Result<ExtensionInfo>;
    async fn remove_extension(&self, name: &str) -> Result<()>;
    async fn list_tools(&self) -> Result<Vec<String>>;
    /// Same set as `list_tools()`, with each tool's extension and description.
    async fn list_tools_detailed(&self) -> Result<Vec<ToolInfo>>;
    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<()>;
}
