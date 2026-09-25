//! Toolkit MCP server: the escape hatch that loads a tool group narrowing left out.
//! Always in the core set, so a mis-scored session can still reach any capability.

use pond_core::mcp::domain::tool_group::TOOLKIT_EXTENSION;
use pond_core::mcp::ports::tools::tool_selection_control::{
    ToolSelectionControl, ToolSelectionError,
};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorData, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ── Parameter structs ─────────────────────────────────────────────────────────

/// Zero real parameters — the schema stays tiny on purpose, because this tool is
/// in every prompt and every character of it is re-prefilled every fresh turn.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListToolGroupsParams {
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct EnableToolGroupParams {
    /// Exact group name, e.g. "giap-schedule".
    pub group: Option<String>,
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// The `group` param, falling back to the aliases a small model tends to invent.
fn resolve_group(params: &EnableToolGroupParams) -> Option<String> {
    if let Some(g) = params.group.as_ref() {
        let g = g.trim();
        if !g.is_empty() {
            return Some(g.to_string());
        }
    }
    for key in ["name", "extension", "tool_group", "group_name"] {
        if let Some(v) = params.extra.get(key).and_then(|v| v.as_str()) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ToolkitMcpServer {
    control: Option<Arc<dyn ToolSelectionControl>>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ToolkitMcpServer {
    /// Every tool this server exposes, without constructing it (`tool_router()` is private).
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new(control: Option<Arc<dyn ToolSelectionControl>>) -> Self {
        Self {
            control,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
List the groups of tools available on this device and whether each is loaded now. \
Use when a capability you need seems to be missing.")]
    async fn list_tool_groups(
        &self,
        ctx: RequestContext<RoleServer>,
        _params: Parameters<ListToolGroupsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("list_tool_groups");
        let Some(control) = &self.control else {
            return Ok(CallToolResult::success(vec![Content::text(
                "All available tools are already loaded for this conversation.",
            )]));
        };
        // `_meta`, not `current_session_id()`: that global is raced by concurrent chat streams.
        let Some(session_id) = crate::session_from_meta(&ctx.meta) else {
            return Ok(CallToolResult::success(vec![Content::text(
                "All available tools are already loaded for this conversation.",
            )]));
        };
        let groups = control.group_status(&session_id).await;
        if groups.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "All available tools are already loaded for this conversation.",
            )]));
        }

        let mut out = String::with_capacity(groups.len() * 96);
        out.push_str("Tool groups on this device:\n");
        for g in &groups {
            let state = if g.core {
                "loaded (always)"
            } else if g.loaded {
                "loaded"
            } else {
                "NOT loaded"
            };
            out.push_str(&format!(
                "- {} [{}] {} tools: {}\n",
                g.extension, state, g.tool_count, g.description
            ));
        }
        out.push_str(
            "\nTo use a group that is NOT loaded, call enable_tool_group with its exact name; \
             its tools become available straight away.",
        );
        Ok(CallToolResult::success(vec![Content::text(out)]))
    }

    #[tool(description = "\
Load a group of tools that is not currently available, by its exact name (e.g. \
\"giap-schedule\"). Its tools can be called immediately afterwards.")]
    async fn enable_tool_group(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<EnableToolGroupParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("enable_tool_group");
        let Some(control) = &self.control else {
            return Ok(CallToolResult::success(vec![Content::text(
                "All available tools are already loaded — there is nothing to enable.",
            )]));
        };
        let Some(group) = resolve_group(&params.0) else {
            return Ok(CallToolResult::success(vec![Content::text(
                "Which group? Call list_tool_groups to see the exact names, then pass one as \
                 'group'.",
            )]));
        };
        // Authorisation: only `_meta` can't be raced, and an unattributed call widens nothing.
        let Some(session_id) = crate::session_from_meta(&ctx.meta) else {
            return Ok(CallToolResult::success(vec![Content::text(
                "Tool groups cannot be changed from here — every group already available \
                 to you is loaded.",
            )]));
        };
        match control.enable_group(&session_id, &group).await {
            Ok(loaded) => {
                tracing::info!(
                    target: "giap::trace",
                    kind = "tool_group_enabled",
                    session_id = %session_id,
                    group = %group,
                    loaded = ?loaded,
                );
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Loaded '{group}'. Its tools are available now — go ahead and call the one you \
                     need. Loaded groups: {}.",
                    loaded.join(", ")
                ))]))
            }
            // Loaded, not yet callable: the catch-all reply would send the model looping.
            Err(ToolSelectionError::NotReady(_)) => {
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Loaded '{group}', but its tools only become callable on your next turn. \
                     Do not call one yet — answer with what you have, or use a tool you \
                     already had."
                ))]))
            }
            // Reported as success: an MCP error makes small models retry the same bad call.
            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Could not load '{group}': {e}. Call list_tool_groups for the exact names."
            ))])),
        }
    }
}

#[tool_handler]
impl ServerHandler for ToolkitMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                TOOLKIT_EXTENSION,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Toolkit MCP server — discover and load tool groups.\n\n\
                 To keep the prompt small on-device, only some groups of tools are loaded for a \
                 conversation. If a capability you need is missing, call list_tool_groups to see \
                 what exists, then enable_tool_group to load it. Nothing is permanently \
                 unavailable.",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct ToolkitDeps {
    control: Option<Arc<dyn ToolSelectionControl>>,
}

static TOOLKIT_DEPS: OnceLock<ToolkitDeps> = OnceLock::new();

/// Install the tool-selection control. Call once, AFTER the agent adapter (its implementor)
/// exists; without it both tools answer "everything is already loaded".
pub fn init_toolkit_deps(control: Option<Arc<dyn ToolSelectionControl>>) {
    let _ = TOOLKIT_DEPS.set(ToolkitDeps { control });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_toolkit_server(reader: DuplexStream, writer: DuplexStream) {
    // No expect(): the extension registers before its implementing adapter is built.
    let control = TOOLKIT_DEPS.get().and_then(|d| d.control.clone());
    let server = ToolkitMcpServer::new(control);
    crate::serve_builtin("giap-toolkit", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A source scan, as rmcp offers no `RequestContext` constructor; comments are stripped.
    #[test]
    fn neither_tool_reads_the_process_global_session() {
        let src = include_str!("toolkit.rs");
        let production = src.split("mod tests").next().unwrap_or(src);
        let code: String = production
            .lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Vacuity control: a rename must not make this pass silently.
        assert!(
            code.contains("session_from_meta("),
            "neither tool resolves a session from _meta — this guard is scanning \
             the wrong thing"
        );
        assert!(
            !code.contains("current_session_id("),
            "toolkit.rs reads the process-global session id. Four chat streams \
             race it, so this widens whichever conversation started a turn most \
             recently rather than the one that called."
        );
    }

    #[test]
    fn group_comes_from_the_declared_param() {
        let params = EnableToolGroupParams {
            group: Some("giap-schedule".to_string()),
            extra: Default::default(),
        };
        assert_eq!(resolve_group(&params), Some("giap-schedule".to_string()));
    }

    #[test]
    fn group_is_recovered_from_common_aliases() {
        for alias in ["name", "extension", "tool_group", "group_name"] {
            let mut extra = std::collections::HashMap::new();
            extra.insert(
                alias.to_string(),
                serde_json::Value::String("giap-vision".to_string()),
            );
            let params = EnableToolGroupParams { group: None, extra };
            assert_eq!(
                resolve_group(&params),
                Some("giap-vision".to_string()),
                "alias '{alias}' was not recovered"
            );
        }
    }

    #[test]
    fn blank_and_absent_groups_resolve_to_none() {
        assert_eq!(resolve_group(&EnableToolGroupParams::default()), None);
        let params = EnableToolGroupParams {
            group: Some("   ".to_string()),
            extra: Default::default(),
        };
        assert_eq!(resolve_group(&params), None);
    }

    /// In "minimal" mode these two tools are the whole surface: 4% of `LOCAL_PROMPT_CLAMP`
    /// (8,192 tokens) at ~4 chars/token on the Gemma template, measured on serialized schemas.
    #[test]
    fn the_hatch_fits_four_percent_of_the_prompt_budget() {
        const PROMPT_BUDGET_TOKENS: usize = 8_192;
        const CHARS_PER_TOKEN: usize = 4;
        let ceiling = PROMPT_BUDGET_TOKENS * 4 / 100 * CHARS_PER_TOKEN;

        let tools = ToolkitMcpServer::tool_router().list_all();
        let chars: usize = tools
            .iter()
            .map(|t| serde_json::to_string(t).map(|s| s.len()).unwrap_or(0))
            .sum();

        assert_eq!(tools.len(), 2, "the hatch is two tools: {tools:?}");
        assert!(
            chars <= ceiling,
            "the minimal tool surface is {chars} chars (~{} tok, {:.1}% of the \
             {PROMPT_BUDGET_TOKENS}-token prompt budget) and the ceiling is \
             {ceiling} chars (327 tok, 4.0%). \"minimal\" no longer delivers \
             what it is named for.",
            chars / CHARS_PER_TOKEN,
            100.0 * (chars as f32 / CHARS_PER_TOKEN as f32) / PROMPT_BUDGET_TOKENS as f32,
        );
    }
}
