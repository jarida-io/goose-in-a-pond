use serde::{Deserialize, Serialize};

use crate::security::domain::secret::SecretRequirement;

/// A curated MCP extension available for one-click installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceExtension {
    /// Unique identifier (e.g. "filesystem", "github").
    pub id: String,
    /// Display name.
    pub name: String,
    pub description: String,
    /// Transport kind: `"stdio"` or `"streamable_http"`.
    pub kind: String,
    /// Stdio: command to run (e.g. `"npx"`).
    #[serde(default)]
    pub command: Option<String>,
    /// Stdio: arguments (e.g. `["-y", "@modelcontextprotocol/server-filesystem", "/"]`).
    #[serde(default)]
    pub args: Vec<String>,
    /// HTTP: URI of the MCP endpoint.
    #[serde(default)]
    pub uri: Option<String>,
    /// Category for filtering (e.g. "productivity", "development", "data").
    pub category: String,
    pub author: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub featured: bool,
    /// A built-in capability that already does this extension's job. While it is on, the
    /// extension only adds tools to every prompt and a process, so the install route refuses it.
    #[serde(default)]
    pub built_in: Option<BuiltInCapability>,
    /// Secrets (API keys, tokens) this extension needs to function.
    #[serde(default)]
    pub required_secrets: Vec<SecretRequirement>,
}

/// A capability the pond ships itself, which a catalogue entry can duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltInCapability {
    /// `giap-knowledge__search_web`, backed by the household's Brave key.
    WebSearch,
}

impl MarketplaceExtension {
    /// The secrets that go into the extension's environment: all but the host-only ones.
    pub fn env_secrets(&self) -> impl Iterator<Item = &SecretRequirement> {
        self.required_secrets.iter().filter(|s| !s.host_only)
    }

    /// The secrets this extension may not be installed or started without.
    pub fn blocking_secrets(&self) -> impl Iterator<Item = &SecretRequirement> {
        self.required_secrets.iter().filter(|s| s.blocks_install())
    }
}
