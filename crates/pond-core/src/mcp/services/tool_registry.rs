//! In-memory tool registry for external MCP extension tools. Starts empty: builtins reach the
//! model as native schemas, so a seed list here could only drift.

use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::mcp::domain::external_tool::ExternalToolDescription;
use crate::mcp::ports::tools::tool_registry::ToolRegistryPort;

/// Maximum description length in compact mode (characters, not bytes).
const COMPACT_DESC_LIMIT: usize = 80;

/// In-memory `ToolRegistryPort`. The `"giap"` key is still read, but nothing populates it.
pub struct InMemoryToolRegistry {
    tools: RwLock<HashMap<String, Vec<ExternalToolDescription>>>,
}

impl InMemoryToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolRegistryPort for InMemoryToolRegistry {
    async fn all_tools(&self) -> Vec<ExternalToolDescription> {
        let map = self.tools.read().await;
        map.values().flat_map(|v| v.iter().cloned()).collect()
    }

    async fn prompt_description_lines(&self, compact: bool) -> Vec<String> {
        let map = self.tools.read().await;
        let mut lines = Vec::new();

        // Built-in tools first (stable ordering for KV-cache prefix reuse)
        if let Some(builtins) = map.get("giap") {
            for tool in builtins {
                let desc = if compact {
                    truncate_description(&tool.description, COMPACT_DESC_LIMIT)
                } else {
                    tool.description.clone()
                };
                lines.push(format!("{} \u{2014} {}", tool.tool_name, desc));
            }
        }

        // External tools, sorted by extension name for determinism
        let mut ext_names: Vec<&String> = map.keys().filter(|k| k.as_str() != "giap").collect();
        ext_names.sort();
        for ext_name in ext_names {
            if let Some(tools) = map.get(ext_name.as_str()) {
                for tool in tools {
                    let desc = if compact {
                        truncate_description(&tool.description, COMPACT_DESC_LIMIT)
                    } else {
                        tool.description.clone()
                    };
                    lines.push(format!(
                        "{}/{} \u{2014} {}",
                        tool.extension_name, tool.tool_name, desc
                    ));
                }
            }
        }

        lines
    }

    async fn register_extension_tools(&self, ext_name: &str, tools: Vec<(String, String)>) {
        let descriptions: Vec<ExternalToolDescription> = tools
            .into_iter()
            .map(|(name, desc)| ExternalToolDescription {
                tool_name: name,
                extension_name: ext_name.to_string(),
                description: desc,
                is_builtin: false,
            })
            .collect();
        let mut map = self.tools.write().await;
        map.insert(ext_name.to_string(), descriptions);
    }

    async fn deregister_extension(&self, ext_name: &str) {
        let mut map = self.tools.write().await;
        map.remove(ext_name);
    }

    async fn resolve_extension(&self, tool_name: &str) -> Option<String> {
        let map = self.tools.read().await;
        for tools in map.values() {
            for tool in tools {
                if tool.tool_name == tool_name {
                    return Some(tool.extension_name.clone());
                }
            }
        }
        None
    }
}

/// Truncate a description to `max_chars` characters, appending "..." if truncated.
fn truncate_description(desc: &str, max_chars: usize) -> String {
    if desc.chars().count() <= max_chars {
        return desc.to_string();
    }
    let truncated: String = desc.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{}...", truncated.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_fresh_registry_advertises_no_builtins() {
        let registry = InMemoryToolRegistry::new();
        assert!(registry.all_tools().await.is_empty());
        assert!(registry.prompt_description_lines(false).await.is_empty());
    }

    #[tokio::test]
    async fn register_external_adds_tools() {
        let registry = InMemoryToolRegistry::new();
        registry
            .register_extension_tools(
                "filesystem",
                vec![
                    ("fs_read".to_string(), "Read a file from disk".to_string()),
                    (
                        "fs_write".to_string(),
                        "Write content to a file".to_string(),
                    ),
                ],
            )
            .await;

        let all = registry.all_tools().await;
        let external: Vec<_> = all.iter().filter(|t| !t.is_builtin).collect();
        assert_eq!(external.len(), 2);
        assert_eq!(external[0].extension_name, "filesystem");
        assert_eq!(external[1].extension_name, "filesystem");
    }

    #[tokio::test]
    async fn deregister_removes_extension_tools() {
        let registry = InMemoryToolRegistry::new();
        registry
            .register_extension_tools(
                "filesystem",
                vec![("fs_read".to_string(), "Read a file".to_string())],
            )
            .await;

        assert!(registry.resolve_extension("fs_read").await.is_some());

        registry.deregister_extension("filesystem").await;

        let all = registry.all_tools().await;
        let external: Vec<_> = all.iter().filter(|t| !t.is_builtin).collect();
        assert_eq!(external.len(), 0);
        assert!(registry.resolve_extension("fs_read").await.is_none());
    }

    #[tokio::test]
    async fn prompt_description_lines_includes_both() {
        let registry = InMemoryToolRegistry::new();
        registry
            .register_extension_tools(
                "filesystem",
                vec![("fs_read".to_string(), "Read a file from disk".to_string())],
            )
            .await;

        let lines = registry.prompt_description_lines(false).await;

        // Only the external tool: builtins travel as native schemas, not prose.
        assert_eq!(lines.len(), 1);

        let last = &lines[lines.len() - 1];
        assert!(
            last.starts_with("filesystem/fs_read"),
            "external tool should have extension prefix: {last}"
        );
    }

    #[tokio::test]
    async fn compact_mode_truncates_descriptions() {
        let registry = InMemoryToolRegistry::new();
        registry
            .register_extension_tools(
                "filesystem",
                vec![(
                    "fs_read".to_string(),
                    "Read a file from disk. Accepts an absolute or relative path, \
                     returns the contents as UTF-8 text, and refuses anything that \
                     does not decode cleanly."
                        .to_string(),
                )],
            )
            .await;

        let full_lines = registry.prompt_description_lines(false).await;
        let compact_lines = registry.prompt_description_lines(true).await;

        assert_eq!(full_lines.len(), compact_lines.len());

        let has_truncated = compact_lines
            .iter()
            .zip(full_lines.iter())
            .any(|(compact, full)| compact.len() < full.len());
        assert!(
            has_truncated,
            "compact mode should truncate at least one description"
        );

        for (compact, full) in compact_lines.iter().zip(full_lines.iter()) {
            if compact.len() < full.len() {
                assert!(
                    compact.ends_with("..."),
                    "truncated line should end with '...': {compact}"
                );
            }
        }
    }

    #[tokio::test]
    async fn resolve_extension_finds_correct_extension() {
        let registry = InMemoryToolRegistry::new();
        registry
            .register_extension_tools(
                "filesystem",
                vec![("fs_read".to_string(), "Read a file".to_string())],
            )
            .await;

        // Builtins are never registered here, so a builtin name resolves to nothing.
        assert_eq!(
            registry.resolve_extension("get_current_weather").await,
            None
        );

        assert_eq!(
            registry.resolve_extension("fs_read").await,
            Some("filesystem".to_string())
        );

        assert_eq!(registry.resolve_extension("nonexistent").await, None);
    }

    #[tokio::test]
    async fn deregistering_an_unknown_extension_leaves_the_others_alone() {
        let registry = InMemoryToolRegistry::new();
        registry
            .register_extension_tools(
                "filesystem",
                vec![("fs_read".to_string(), "Read a file from disk".to_string())],
            )
            .await;

        registry.deregister_extension("never-registered").await;

        assert_eq!(registry.all_tools().await.len(), 1);
    }

    #[test]
    fn truncate_description_short_unchanged() {
        assert_eq!(truncate_description("short", 80), "short");
    }

    #[test]
    fn truncate_description_long_truncated() {
        let long = "a".repeat(100);
        let result = truncate_description(&long, 80);
        assert!(result.ends_with("..."));
        assert!(result.chars().count() <= 80);
    }

    #[test]
    fn truncate_description_exact_boundary() {
        let exact = "a".repeat(80);
        assert_eq!(truncate_description(&exact, 80), exact);
    }
}
