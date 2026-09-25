// ── Modular MCP servers ─────────────────────────────────────────────────────
pub mod context;
pub mod device;
pub mod device_control;
pub mod dispatcher;
pub mod knowledge;
pub mod memory;
pub mod orchestrator;
pub mod schedule;
pub mod secrets;
pub mod sensors;
pub mod session_meta;
pub mod system;
pub mod toolkit;
pub mod weather;
pub mod wolfram;

// ── Shared utilities for Knowledge-family servers ───────────────────────────
pub mod format;
pub mod http;
/// The tool block as the model sees it, pinned so a moved KV prefix is a diff.
pub mod prefix_oracle;

// ── Shared state for tool param generation ──────────────────────────────────

use pond_core::mcp::ports::tools::tool_caller::ToolCaller;
use std::sync::{Arc, OnceLock, RwLock};

// -- User message and current session: set by GooseAdapter before each turn --
static LAST_USER_MESSAGE: RwLock<String> = RwLock::new(String::new());

// In `pond-core` so outboard adapters can report egress without depending on this crate.
pub use pond_core::shared::services::egress::{
    current_session_id, current_tool, set_current_session_id, set_current_tool, set_egress_sink,
};

pub fn set_last_user_message(msg: &str) {
    if let Ok(mut guard) = LAST_USER_MESSAGE.write() {
        *guard = msg.to_string();
    }
}

pub fn last_user_message() -> String {
    LAST_USER_MESSAGE
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

// -- ToolCaller specialist: the main LLM picks WHEN to call a tool, this picks the params --
static TOOL_CALLER: OnceLock<Option<Arc<dyn ToolCaller>>> = OnceLock::new();

/// Set the tool-calling specialist. Call once at startup.
pub fn set_tool_caller(tc: Option<Arc<dyn ToolCaller>>) {
    let _ = TOOL_CALLER.set(tc);
}

pub fn tool_caller() -> Option<Arc<dyn ToolCaller>> {
    TOOL_CALLER.get().and_then(|opt| opt.clone())
}

// -- Notification sender: lets `send_notification` push to phones; unset → desktop-only --
static NOTIFICATION_SENDER: OnceLock<
    Arc<dyn pond_core::mcp::ports::notification::NotificationSender>,
> = OnceLock::new();

/// Install the notification sender. Call once at startup.
pub fn init_notification_sender(
    sender: Arc<dyn pond_core::mcp::ports::notification::NotificationSender>,
) {
    let _ = NOTIFICATION_SENDER.set(sender);
}

pub fn notification_sender(
) -> Option<Arc<dyn pond_core::mcp::ports::notification::NotificationSender>> {
    NOTIFICATION_SENDER.get().cloned()
}

/// Generate tool params via the ToolCaller; when one is configured they replace the LLM's.
pub async fn generate_params(
    tool_name: &str,
    schema: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let tc = tool_caller()?;
    let user_msg = last_user_message();
    if user_msg.is_empty() {
        eprintln!(
            "[tool-caller] {} skipped: no user message available",
            tool_name
        );
        return None;
    }
    eprintln!("[tool-caller] ╔═══ ToolCaller Request ═══");
    eprintln!("[tool-caller] ║ tool:   {}", tool_name);
    eprintln!("[tool-caller] ║ query:  {:?}", user_msg);
    eprintln!("[tool-caller] ║ schema: {}", schema);
    eprintln!("[tool-caller] ╚═════════════════════════");

    match tc.generate_tool_call(tool_name, schema, &user_msg).await {
        Ok(args) if !args.is_empty() => {
            eprintln!("[tool-caller] ╔═══ ToolCaller Response ═══");
            eprintln!("[tool-caller] ║ status: SUCCESS");
            for (k, v) in &args {
                eprintln!("[tool-caller] ║ {}: {}", k, v);
            }
            eprintln!("[tool-caller] ╚══════════════════════════");
            Some(args)
        }
        Ok(_) => {
            eprintln!("[tool-caller] ╔═══ ToolCaller Response ═══");
            eprintln!("[tool-caller] ║ status: EMPTY (no args returned)");
            eprintln!("[tool-caller] ╚══════════════════════════");
            None
        }
        Err(e) => {
            eprintln!("[tool-caller] ╔═══ ToolCaller Response ═══");
            eprintln!("[tool-caller] ║ status: FAILED");
            eprintln!("[tool-caller] ║ error:  {e}");
            eprintln!("[tool-caller] ╚══════════════════════════");
            None
        }
    }
}

pub use device::DeviceMcpServer;
pub use device_control::DeviceControlMcpServer;
pub use knowledge::{clean_query_for_search, KnowledgeMcpServer};
pub use memory::{auto_classify_segment, parse_memory_segment, parse_memory_tier, MemoryMcpServer};
pub use orchestrator::OrchestratorMcpServer;
pub use schedule::{try_upcoming_schedules_context, ScheduleMcpServer};
pub use sensors::SensorsMcpServer;
pub use system::SystemMcpServer;
pub use toolkit::ToolkitMcpServer;
pub use weather::WeatherMcpServer;

pub use device::{init_device_deps, spawn_device_server};
pub use device_control::{init_device_control_deps, spawn_device_control_server};
pub use knowledge::{init_knowledge_deps, spawn_knowledge_server};
pub use memory::{init_memory_deps, spawn_memory_server};
// Installed by pond-server once the Goose adapter exists, not by `register_giap_extensions`.
pub use orchestrator::{
    init_orchestrator_deps, installed_orchestrator_deps, spawn_orchestrator_server,
    OrchestratorDeps,
};
pub use schedule::{init_schedule_deps, spawn_schedule_server};
pub use secrets::{init_secret_deps, secret};
pub use sensors::{init_sensor_deps, spawn_sensor_server};
pub use session_meta::{session_from_meta, SESSION_ID_META_KEY};
pub use system::spawn_system_server;
pub use toolkit::{init_toolkit_deps, spawn_toolkit_server};
pub use weather::{init_weather_deps, spawn_weather_server, WEATHER_APP_URI};

// ── MCP App resources ─────────────────────────────────────────────────────

/// All embedded MCP App resources as `(uri, html_content)` pairs, for pond-api's registry.
pub fn all_app_resources() -> Vec<(&'static str, &'static str)> {
    let mut resources = Vec::new();
    resources.extend(weather::app_resources());
    resources
}

pub use format::{
    format_api_error, format_dead_end, format_list_result, format_no_results,
    format_not_configured, truncate_to_budget,
};
pub use http::{build_http_client, traced_get, traced_get_with};

/// Serve one builtin MCP server on a duplex pair and log when it stops. No restart: `serve`
/// consumes the halves and only goose's extension manager can hand out a fresh pair.
pub fn serve_builtin<S>(
    extension: &'static str,
    server: S,
    reader: tokio::io::DuplexStream,
    writer: tokio::io::DuplexStream,
) where
    S: rmcp::ServerHandler + Send + 'static,
{
    use rmcp::ServiceExt;
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => match running.waiting().await {
                Ok(reason) => tracing::warn!(
                    target: "giap::trace",
                    kind = "mcp_server_stopped",
                    extension,
                    ?reason,
                    "builtin MCP server stopped; its tools now fail for the rest of this process"
                ),
                Err(e) => tracing::error!(
                    target: "giap::trace",
                    kind = "mcp_server_stopped",
                    extension,
                    error = %e,
                    "builtin MCP server ended abnormally; its tools now fail for the rest of this process"
                ),
            },
            Err(e) => tracing::error!(
                target: "giap::trace",
                kind = "mcp_server_failed",
                extension,
                error = %e,
                "builtin MCP server failed to start"
            ),
        }
    });
}

pub use dispatcher::McpToolDispatcher;
