use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Persisted configuration for one external MCP server connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique stable identifier (UUID).
    pub id: String,
    /// Human-readable name used as the extension key in Goose (e.g. "filesystem").
    pub name: String,
    /// Transport kind: `"stdio"` or `"streamable_http"`.
    pub kind: String,
    /// Optional description shown in the UI.
    pub description: String,
    /// Stdio: executable command (e.g. `"npx"`).
    pub command: Option<String>,
    /// Stdio: arguments passed to the command.
    pub args: Vec<String>,
    /// Stdio: environment variables injected into the child process.
    pub env: HashMap<String, String>,
    /// HTTP: full URI of the MCP endpoint (e.g. `"http://localhost:3000/mcp"`).
    pub uri: Option<String>,
    /// When `false` the server is skipped on startup (soft-disable without deleting).
    pub enabled: bool,
    /// ISO-8601 timestamp when the record was created.
    pub created_at: String,
}

/// Persistent storage for configured external MCP server connections.
#[async_trait]
pub trait McpServerRepository: Send + Sync {
    /// Return all saved MCP server configurations (enabled and disabled).
    async fn list(&self) -> Result<Vec<McpServerConfig>>;

    /// Insert or replace a server configuration (upsert by `name`).
    async fn save(&self, config: &McpServerConfig) -> Result<()>;

    /// Removes the config named `name`; `Ok(())` even if no row matched.
    async fn delete(&self, name: &str) -> Result<()>;

    /// Sets the `enabled` flag for `name`; `Ok(())` even if no row matched.
    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<()>;
}
