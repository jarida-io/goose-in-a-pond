//! McpToolDispatcher — routes tool calls directly to GIAP's builtin MCP servers.
//!
//! Implements [`ToolDispatcher`] by holding instances of all MCP servers and
//! routing calls by tool name prefix (e.g. "giap-weather__get_current_weather").
//! This bypasses Goose's extension manager, enabling the PondAgent to call
//! GIAP tools without a running Goose session.
//!
//! # Peer Construction
//!
//! rmcp's `RequestContext<RoleServer>` requires a `Peer` which can only be
//! obtained from a running service. We use `serve_directly_with_ct` with a
//! DuplexStream to create a minimal background service, extract the peer,
//! and reuse it for all tool dispatch calls. Since GIAP's MCP handlers never
//! access `ctx.peer`, the peer's transport being a no-op is safe.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_adapters_weather::WeatherProvider;
use pond_core::mcp::ports::tools::tool_dispatcher::{ToolCallResult, ToolDispatcher};
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::draft::DraftRepository;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::recipe::AgentRecipeRepository;
use pond_core::user_data::ports::scheduler::SchedulerPort;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::skill::UserSkillRepository;
use rmcp::model::{CallToolRequestParams, CallToolResult as RmcpCallToolResult, RequestId};
use rmcp::service::{Peer, RequestContext, RunningService};
use rmcp::{RoleServer, ServerHandler};
use std::sync::Arc;

use crate::{
    DeviceMcpServer, DiscoveryMcpServer, DraftMcpServer, FinanceMcpServer, KnowledgeMcpServer,
    MemoryMcpServer, NewsMcpServer, ScheduleMcpServer, SystemMcpServer, WeatherMcpServer,
};

// ── Tool name constants ──────────────────────────────────────────────────────

const PREFIX_WEATHER: &str = "giap-weather__";
const PREFIX_KNOWLEDGE: &str = "giap-knowledge__";
const PREFIX_MEMORY: &str = "giap-memory__";
const PREFIX_SCHEDULE: &str = "giap-schedule__";
const PREFIX_SYSTEM: &str = "giap-system__";
const PREFIX_DEVICE: &str = "giap-device__";
const PREFIX_NEWS: &str = "giap-news__";
const PREFIX_FINANCE: &str = "giap-finance__";
const PREFIX_DISCOVERY: &str = "giap-discovery__";
const PREFIX_DRAFT: &str = "giap-draft__";

// No hardcoded tool list — all tools discovered dynamically via ServerHandler::list_tools().

// ── Dispatcher ───────────────────────────────────────────────────────────────

/// A registered MCP server with its tool-name prefix.
struct RegisteredServer {
    prefix: &'static str,
    server: Box<dyn McpServerBridge>,
}

/// Object-safe bridge for calling MCP server methods we need.
/// Wraps rmcp's `ServerHandler` (which isn't object-safe due to `impl Future`).
#[async_trait]
trait McpServerBridge: Send + Sync {
    async fn list_tools_bridged(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Vec<(String, String, serde_json::Value)>;

    async fn call_tool_bridged(
        &self,
        params: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<RmcpCallToolResult>;
}

/// Blanket impl for any rmcp ServerHandler.
#[async_trait]
impl<T: ServerHandler + Send + Sync> McpServerBridge for T {
    async fn list_tools_bridged(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Vec<(String, String, serde_json::Value)> {
        match ServerHandler::list_tools(self, None, ctx).await {
            Ok(result) => result
                .tools
                .into_iter()
                .map(|t| {
                    let name = t.name.to_string();
                    let desc = t.description.map(|d| d.to_string()).unwrap_or_default();
                    let schema = serde_json::Value::Object(t.input_schema.as_ref().clone());
                    (name, desc, schema)
                })
                .collect(),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    type_name = std::any::type_name::<T>(),
                    "MCP server list_tools failed — its tools will be missing"
                );
                Vec::new()
            }
        }
    }

    async fn call_tool_bridged(
        &self,
        params: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<RmcpCallToolResult> {
        ServerHandler::call_tool(self, params, ctx)
            .await
            .map_err(|e| anyhow!("MCP call_tool failed: {}", e))
    }
}

/// Concrete dispatcher that routes tool calls to GIAP's builtin MCP servers.
///
/// Servers are registered dynamically — adding a new MCP server only requires
/// pushing it into the `servers` vec with its prefix. Tool schemas are queried
/// from each server at runtime via `list_tools()`, not hardcoded.
pub struct McpToolDispatcher {
    servers: Vec<RegisteredServer>,
    /// Cloned peer from a minimal running service — used to construct RequestContext.
    peer: Peer<RoleServer>,
    /// Keeps the background peer-provider service alive. Dropped on dispatcher drop.
    _peer_service: RunningService<RoleServer, SystemMcpServer>,
}

impl McpToolDispatcher {
    /// Construct the dispatcher with the same dependencies used by
    /// `register_giap_extensions()`.
    ///
    /// This spawns a minimal background task to maintain a valid rmcp `Peer`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        memory_repo: Arc<dyn MemoryRepository>,
        weather: Option<Arc<dyn WeatherProvider>>,
        scheduler: Option<Arc<dyn SchedulerPort>>,
        settings_repo: Arc<dyn SettingsRepository>,
        device_registry: Arc<dyn DeviceRegistry>,
        skill_repo: Arc<dyn UserSkillRepository>,
        recipe_repo: Arc<dyn AgentRecipeRepository>,
        draft_repo: Arc<dyn DraftRepository>,
        embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    ) -> Self {
        let http_client = crate::build_http_client();

        let weather_server = WeatherMcpServer::new(weather);
        let knowledge_server = KnowledgeMcpServer::new(http_client.clone());
        let memory_server = MemoryMcpServer::new(memory_repo, embedding_provider);
        let schedule_server = scheduler.map(|s| ScheduleMcpServer::new(s, settings_repo.clone()));
        let system_server = SystemMcpServer::new();
        let device_server = DeviceMcpServer::new(
            device_registry,
            settings_repo.clone(),
            skill_repo,
            recipe_repo,
        );
        let news_server = NewsMcpServer::new(http_client.clone(), settings_repo.clone());
        let finance_server = FinanceMcpServer::new(http_client.clone(), settings_repo.clone());
        let discovery_server = DiscoveryMcpServer::new(http_client, settings_repo);
        let draft_server = DraftMcpServer::new(draft_repo);

        // Create a dummy peer via serve_directly on a DuplexStream.
        // The system server is lightweight (no deps) — we use it as the service
        // backing the peer. The DuplexStream client side is immediately dropped
        // so no real traffic flows; we just need the Peer for RequestContext.
        let (_client_stream, server_stream) = tokio::io::duplex(64);
        let running = rmcp::service::serve_directly(SystemMcpServer::new(), server_stream, None);
        let peer = running.peer().clone();

        // Register all servers dynamically. Adding a new MCP server only
        // requires pushing it here — schemas come from list_tools() automatically.
        let mut servers: Vec<RegisteredServer> = vec![
            RegisteredServer {
                prefix: PREFIX_WEATHER,
                server: Box::new(weather_server),
            },
            RegisteredServer {
                prefix: PREFIX_KNOWLEDGE,
                server: Box::new(knowledge_server),
            },
            RegisteredServer {
                prefix: PREFIX_MEMORY,
                server: Box::new(memory_server),
            },
            RegisteredServer {
                prefix: PREFIX_SYSTEM,
                server: Box::new(system_server),
            },
            RegisteredServer {
                prefix: PREFIX_DEVICE,
                server: Box::new(device_server),
            },
            RegisteredServer {
                prefix: PREFIX_NEWS,
                server: Box::new(news_server),
            },
            RegisteredServer {
                prefix: PREFIX_FINANCE,
                server: Box::new(finance_server),
            },
            RegisteredServer {
                prefix: PREFIX_DISCOVERY,
                server: Box::new(discovery_server),
            },
            RegisteredServer {
                prefix: PREFIX_DRAFT,
                server: Box::new(draft_server),
            },
        ];
        if let Some(sched) = schedule_server {
            servers.push(RegisteredServer {
                prefix: PREFIX_SCHEDULE,
                server: Box::new(sched),
            });
        }

        Self {
            servers,
            peer,
            _peer_service: running,
        }
    }

    /// Build a `RequestContext<RoleServer>` for dispatching a tool call.
    ///
    /// Uses the shared peer. GIAP handlers never access the peer so the
    /// closed transport is irrelevant.
    fn make_context(&self) -> RequestContext<RoleServer> {
        RequestContext::new(RequestId::Number(0), self.peer.clone())
    }
}

#[async_trait]
impl ToolDispatcher for McpToolDispatcher {
    async fn dispatch(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolCallResult> {
        let (server_prefix, bare_name) = parse_tool_name(tool_name)?;

        // Find the server that owns this prefix.
        let server = self
            .servers
            .iter()
            .find(|s| s.prefix == server_prefix)
            .ok_or_else(|| anyhow!("No server registered for prefix '{}'", server_prefix))?;

        // Convert arguments to rmcp format.
        let args_map = match arguments {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            _ => Some(serde_json::Map::from_iter([(
                "input".to_string(),
                arguments,
            )])),
        };

        let params = if let Some(args) = args_map {
            CallToolRequestParams::new(bare_name.to_string()).with_arguments(args)
        } else {
            CallToolRequestParams::new(bare_name.to_string())
        };

        let ctx = self.make_context();
        match server.server.call_tool_bridged(params, ctx).await {
            Ok(rmcp_result) => Ok(convert_rmcp_result(rmcp_result)),
            Err(e) => Ok(ToolCallResult {
                content: e.to_string(),
                success: false,
            }),
        }
    }

    async fn available_tools(&self) -> Vec<String> {
        // Dynamically query all registered servers.
        let mut tools = Vec::new();
        for reg in &self.servers {
            let ctx = self.make_context();
            let defs = reg.server.list_tools_bridged(ctx).await;
            for (name, _, _) in defs {
                tools.push(format!("{}{}", reg.prefix, name));
            }
        }
        tools
    }

    async fn available_tool_definitions(&self) -> Vec<(String, String, serde_json::Value)> {
        // Query each registered server for its tool schemas dynamically.
        // Adding a new MCP server only requires pushing it in the constructor —
        // schemas come from list_tools() automatically, no hardcoding needed.
        let mut all_defs = Vec::new();
        for reg in &self.servers {
            let ctx = self.make_context();
            let defs = reg.server.list_tools_bridged(ctx).await;
            let count = defs.len();
            if count > 0 {
                // Log ALL tools with their full schemas so you can verify what the model sees.
                for (name, desc, schema) in &defs {
                    tracing::debug!(
                        prefix = reg.prefix,
                        tool = %name,
                        description = %desc,
                        schema = %serde_json::to_string(schema).unwrap_or_default(),
                        "tool schema"
                    );
                }
            } else {
                tracing::warn!(prefix = reg.prefix, "server returned 0 tool definitions");
            }
            for (bare_name, desc, schema) in defs {
                let full_name = format!("{}{}", reg.prefix, bare_name);
                all_defs.push((full_name, desc, schema));
            }
        }
        tracing::info!(
            total = all_defs.len(),
            "total tool definitions collected from MCP servers"
        );
        all_defs
    }
}

// Removed: tool_param_schema() — schemas now come dynamically from ServerHandler::list_tools().
// This dead code is kept temporarily for reference during the transition.
#[allow(dead_code)]
fn _tool_param_schema_removed(tool_name: &str) -> serde_json::Value {
    use serde_json::json;
    match tool_name {
        // Weather
        "giap-weather__get_current_weather" => json!({
            "type": "object",
            "properties": {
                "location": { "type": "string", "description": "City name (e.g. 'Nairobi', 'London'). Omit for home location." }
            }
        }),
        "giap-weather__get_weather_forecast" => json!({
            "type": "object",
            "properties": {
                "location": { "type": "string", "description": "City name. Omit for home location." },
                "days": { "type": "integer", "description": "Number of days (1-7, default 3)." }
            }
        }),
        // Knowledge
        "giap-knowledge__get_wikipedia_article" => json!({
            "type": "object",
            "properties": {
                "topic": { "type": "string", "description": "The person, place, event, or concept to look up." }
            },
            "required": ["topic"]
        }),
        "giap-knowledge__search_wikipedia" => json!({
            "type": "object",
            "properties": {
                "topic": { "type": "string", "description": "Search query for Wikipedia." }
            },
            "required": ["topic"]
        }),
        "giap-knowledge__instant_answer" => json!({
            "type": "object",
            "properties": {
                "topic": { "type": "string", "description": "The question or topic to get a quick answer for." }
            },
            "required": ["topic"]
        }),
        "giap-knowledge__define_word" => json!({
            "type": "object",
            "properties": {
                "word": { "type": "string", "description": "The word to define." }
            },
            "required": ["word"]
        }),
        "giap-knowledge__search_books" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Book title, author, or topic to search for." }
            },
            "required": ["query"]
        }),
        // Memory
        "giap-memory__save_memory" => json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "The fact, preference, or note to save." },
                "segment": { "type": "string", "description": "Category: identity, preference, correction, relationship, project, knowledge, context." }
            },
            "required": ["content"]
        }),
        "giap-memory__recall_memories" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "What to search for in saved memories." }
            },
            "required": ["query"]
        }),
        "giap-memory__forget_memory" => json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "The memory ID to delete." }
            },
            "required": ["id"]
        }),
        // Schedule
        "giap-schedule__create_schedule" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Human-readable name for the task." },
                "cron": { "type": "string", "description": "6-field cron: sec min hr dom mon dow. E.g. '0 0 8 * * *' for daily 8 AM." },
                "prompt": { "type": "string", "description": "The prompt to run on each fire." },
                "timezone": { "type": "string", "description": "IANA timezone (e.g. 'Africa/Nairobi')." }
            },
            "required": ["name", "cron", "prompt"]
        }),
        "giap-schedule__list_schedules" => json!({ "type": "object", "properties": {} }),
        "giap-schedule__update_schedule" => json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Schedule ID to update." },
                "name": { "type": "string" },
                "cron": { "type": "string" },
                "prompt": { "type": "string" },
                "timezone": { "type": "string" }
            },
            "required": ["id"]
        }),
        "giap-schedule__delete_schedule"
        | "giap-schedule__pause_schedule"
        | "giap-schedule__resume_schedule"
        | "giap-schedule__run_schedule_now" => json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Schedule ID." }
            },
            "required": ["id"]
        }),
        "giap-schedule__get_schedule_runs" => json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Schedule ID." },
                "limit": { "type": "integer", "description": "Max runs to return (default 10)." }
            },
            "required": ["id"]
        }),
        "giap-schedule__world_clock" => json!({
            "type": "object",
            "properties": {
                "timezones": { "type": "array", "items": { "type": "string" }, "description": "IANA timezone names. Omit for user's timezone." }
            }
        }),
        // System
        "giap-system__get_current_time" => json!({ "type": "object", "properties": {} }),
        "giap-system__get_system_info" => json!({ "type": "object", "properties": {} }),
        "giap-system__send_notification" => json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Notification title." },
                "body": { "type": "string", "description": "Notification body text." }
            },
            "required": ["title", "body"]
        }),
        "giap-system__run_shell_command" => json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to execute (must be in the allow-list)." }
            },
            "required": ["command"]
        }),
        "giap-system__read_file" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the file to read." }
            },
            "required": ["path"]
        }),
        "giap-system__write_file" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the file." },
                "content": { "type": "string", "description": "Content to write." },
                "append": { "type": "boolean", "description": "If true, append instead of overwrite." }
            },
            "required": ["path", "content"]
        }),
        // Device
        "giap-device__list_registered_devices"
        | "giap-device__get_user_profile"
        | "giap-device__get_model_assignments"
        | "giap-device__list_skills" => {
            json!({ "type": "object", "properties": {} })
        }
        "giap-device__get_recipe" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Recipe name to look up." }
            },
            "required": ["name"]
        }),
        // News
        "giap-news__get_top_stories" => json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "description": "Number of stories (default 5)." }
            }
        }),
        "giap-news__search_news" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Topic to search for in news." }
            },
            "required": ["query"]
        }),
        "giap-news__get_headlines" => json!({
            "type": "object",
            "properties": {
                "category": { "type": "string", "description": "News category (e.g. 'technology', 'sports', 'business')." }
            }
        }),
        // Finance
        "giap-finance__get_exchange_rate" => json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "description": "Source currency code (e.g. 'USD')." },
                "to": { "type": "string", "description": "Target currency code (e.g. 'KES')." }
            },
            "required": ["from", "to"]
        }),
        "giap-finance__convert_currency" => json!({
            "type": "object",
            "properties": {
                "amount": { "type": "number", "description": "Amount to convert." },
                "from": { "type": "string", "description": "Source currency code." },
                "to": { "type": "string", "description": "Target currency code." }
            },
            "required": ["amount", "from", "to"]
        }),
        "giap-finance__get_stock_quote" => json!({
            "type": "object",
            "properties": {
                "symbol": { "type": "string", "description": "Stock ticker symbol (e.g. 'AAPL', 'MSFT')." }
            },
            "required": ["symbol"]
        }),
        "giap-finance__get_crypto_price" => json!({
            "type": "object",
            "properties": {
                "coin": { "type": "string", "description": "Cryptocurrency name or ID (e.g. 'bitcoin', 'ethereum')." }
            },
            "required": ["coin"]
        }),
        // Discovery
        "giap-discovery__get_country_info" => json!({
            "type": "object",
            "properties": {
                "country": { "type": "string", "description": "Country name (e.g. 'Kenya', 'Japan')." }
            },
            "required": ["country"]
        }),
        "giap-discovery__lookup_product" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Product name or barcode." }
            },
            "required": ["query"]
        }),
        "giap-discovery__get_product_price" => json!({
            "type": "object",
            "properties": {
                "product": { "type": "string", "description": "Product name to get price for." }
            },
            "required": ["product"]
        }),
        "giap-discovery__search_web" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query." }
            },
            "required": ["query"]
        }),
        // Draft
        "giap-draft__save_draft" => json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "Draft content to save for approval." },
                "title": { "type": "string", "description": "Draft title." }
            },
            "required": ["content"]
        }),
        "giap-draft__list_drafts" => json!({ "type": "object", "properties": {} }),
        "giap-draft__approve_draft" | "giap-draft__reject_draft" => json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Draft ID." }
            },
            "required": ["id"]
        }),
        // Fallback
        _ => json!({ "type": "object", "properties": {} }),
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Parse a tool name like "giap-weather__get_current_weather" into
/// (prefix = "giap-weather__", bare_name = "get_current_weather").
fn parse_tool_name(tool_name: &str) -> Result<(&str, &str)> {
    if let Some(pos) = tool_name.find("__") {
        let prefix = &tool_name[..pos + 2]; // include the "__"
        let bare = &tool_name[pos + 2..];
        if bare.is_empty() {
            return Err(anyhow!("Empty tool name after prefix: {}", tool_name));
        }
        Ok((prefix, bare))
    } else {
        Err(anyhow!(
            "Invalid tool name format (expected 'giap-<server>__<tool>'): {}",
            tool_name
        ))
    }
}

/// Extract text content from rmcp's CallToolResult.
fn convert_rmcp_result(result: RmcpCallToolResult) -> ToolCallResult {
    let success = !result.is_error.unwrap_or(false);
    let content = result
        .content
        .into_iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");

    ToolCallResult { content, success }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_weather_tool_name() {
        let (prefix, bare) = parse_tool_name("giap-weather__get_current_weather").unwrap();
        assert_eq!(prefix, "giap-weather__");
        assert_eq!(bare, "get_current_weather");
    }

    #[test]
    fn parse_schedule_tool_name() {
        let (prefix, bare) = parse_tool_name("giap-schedule__create_schedule").unwrap();
        assert_eq!(prefix, "giap-schedule__");
        assert_eq!(bare, "create_schedule");
    }

    #[test]
    fn parse_invalid_tool_name() {
        let result = parse_tool_name("no_prefix_tool");
        assert!(result.is_err());
    }

    #[test]
    fn parse_empty_bare_name() {
        let result = parse_tool_name("giap-weather__");
        assert!(result.is_err());
    }

    #[test]
    fn parse_tool_name_with_ext_prefix() {
        // External MCP servers use "ext-{name}__" prefix format.
        let (prefix, bare) = parse_tool_name("ext-filesystem__read_file").unwrap();
        assert_eq!(prefix, "ext-filesystem__");
        assert_eq!(bare, "read_file");
    }

    /// Diagnostic: call list_tools on ALL MCP servers and show what the dispatcher collects.
    /// Run with `cargo test -p pond-mcp-server -- --nocapture inspect_mcp_tool_schemas`
    #[tokio::test]
    async fn inspect_mcp_tool_schemas() {
        use crate::{
            DiscoveryMcpServer, DraftMcpServer, FinanceMcpServer, KnowledgeMcpServer,
            NewsMcpServer, SystemMcpServer, WeatherMcpServer,
        };

        // Create peer for RequestContext
        let (_client, server_stream) = tokio::io::duplex(64);
        let running = rmcp::service::serve_directly(SystemMcpServer::new(), server_stream, None);
        let peer = running.peer().clone();

        let http_client = crate::build_http_client();

        // Mock settings repo
        struct MockSettings;
        #[async_trait]
        impl pond_core::user_data::ports::settings::SettingsRepository for MockSettings {
            async fn get(
                &self,
            ) -> anyhow::Result<pond_core::user_data::domain::settings::Settings> {
                Ok(pond_core::user_data::domain::settings::Settings::default())
            }
            async fn update(
                &self,
                _: &pond_core::user_data::domain::settings::Settings,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn get_key(&self, _: &str) -> anyhow::Result<Option<String>> {
                Ok(None)
            }
            async fn set_key(&self, _: &str, _: String) -> anyhow::Result<()> {
                Ok(())
            }
        }
        let settings: Arc<dyn pond_core::user_data::ports::settings::SettingsRepository> =
            Arc::new(MockSettings);

        // All servers that don't require complex real deps
        let servers: Vec<(&str, Box<dyn McpServerBridge>)> = vec![
            (
                "giap-system__",
                Box::new(SystemMcpServer::new()) as Box<dyn McpServerBridge>,
            ),
            ("giap-weather__", Box::new(WeatherMcpServer::new(None))),
            (
                "giap-knowledge__",
                Box::new(KnowledgeMcpServer::new(http_client.clone())),
            ),
            (
                "giap-news__",
                Box::new(NewsMcpServer::new(http_client.clone(), settings.clone())),
            ),
            (
                "giap-finance__",
                Box::new(FinanceMcpServer::new(http_client.clone(), settings.clone())),
            ),
            (
                "giap-discovery__",
                Box::new(DiscoveryMcpServer::new(
                    http_client.clone(),
                    settings.clone(),
                )),
            ),
        ];

        println!("\n=== FULL MCP TOOL SCHEMA REPORT ===\n");
        let mut total_tools = 0;
        let mut all_tools_json = Vec::new();

        for (prefix, server) in &servers {
            let ctx = RequestContext::new(RequestId::Number(0), peer.clone());
            let defs = server.list_tools_bridged(ctx).await;
            println!("[{}] {} tools", prefix, defs.len());
            for (name, _desc, schema) in &defs {
                let has_props = schema.get("properties").is_some();
                let has_type = schema.get("type").is_some();
                println!(
                    "  {} — has_properties={}, has_type={}",
                    name, has_props, has_type
                );

                // Build OpenAI format (same as tools_to_json in pond-inference)
                all_tools_json.push(serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": format!("{}{}", prefix, name),
                        "description": _desc,
                        "parameters": schema,
                    }
                }));
            }
            total_tools += defs.len();
        }

        println!("\n=== TOTAL: {} tools ===", total_tools);
        println!("\n=== FIRST 2 TOOLS IN OPENAI FORMAT (what Jinja receives) ===\n");
        for tool in all_tools_json.iter().take(2) {
            println!("{}\n", serde_json::to_string_pretty(tool).unwrap());
        }

        assert!(total_tools >= 24, "Expected 24+ tools, got {}", total_tools);
    }
}
