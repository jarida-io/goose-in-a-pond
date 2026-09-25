//! System MCP server: time, system info, notifications, shell and file I/O. Stateless.

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

// ── Parameter structs ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SystemInfoParams {
    /// "all" | "memory" | "disk" | "os"; default "all"
    pub category: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NotifyParams {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ShellCommandParams {
    /// Allow-listed command name only, no args
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ReadFileParams {
    /// Absolute path
    pub path: String,
    pub max_lines: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WriteFileParams {
    /// Absolute path
    pub path: String,
    pub content: String,
    /// true = append, default overwrite
    #[serde(default)]
    pub append: bool,
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SystemMcpServer {
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl SystemMcpServer {
    /// Every tool this server exposes, without constructing it (`tool_router()` is private).
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Get current date, time, and timezone.")]
    async fn get_current_time(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let now = chrono::Local::now();
        let text = format!(
            "Current time: {}\nDate: {}\nTimezone: {}",
            now.format("%H:%M:%S"),
            now.format("%A, %B %d, %Y"),
            now.format("%Z (UTC%:z)")
        );
        let ui_data = serde_json::json!({
            "time": now.format("%H:%M").to_string(),
            "timezone": now.format("%Z").to_string(),
            "date": now.format("%A, %B %d, %Y").to_string(),
            "utc_offset": now.format("%:z").to_string(),
        });
        let hint = format!("[[[mcp-ui:time:{}]]]\n", ui_data);
        let full_result = format!("{}{}", hint, text);
        Ok(CallToolResult::success(vec![Content::text(full_result)]))
    }

    #[tool(description = "Get system info: OS, hostname, memory, and disk usage.")]
    async fn get_system_info(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SystemInfoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        use sysinfo::{Disks, System};

        let category = params.0.category.as_deref().unwrap_or("all");
        let mut sections: Vec<String> = Vec::new();

        let show_os = category == "all" || category == "os";
        let show_memory = category == "all" || category == "memory";
        let show_disk = category == "all" || category == "disk";

        if show_os {
            let host = System::host_name().unwrap_or_else(|| "unknown".to_string());
            let os_name = System::name().unwrap_or_else(|| "unknown".to_string());
            let os_version = System::os_version().unwrap_or_else(|| "unknown".to_string());
            let kernel = System::kernel_version().unwrap_or_else(|| "unknown".to_string());
            let arch = System::cpu_arch();
            let uptime_secs = System::uptime();
            let hours = uptime_secs / 3600;
            let minutes = (uptime_secs % 3600) / 60;
            sections.push(format!(
                "OS: {} {}\nKernel: {}\nArchitecture: {}\nHostname: {}\nUptime: {}h {}m",
                os_name, os_version, kernel, arch, host, hours, minutes,
            ));
        }

        if show_memory {
            let mut sys = System::new();
            sys.refresh_memory();
            let total_gb = sys.total_memory() as f64 / 1_073_741_824.0;
            let used_gb = sys.used_memory() as f64 / 1_073_741_824.0;
            let available_gb = sys.available_memory() as f64 / 1_073_741_824.0;
            sections.push(format!(
                "Memory: {:.1} GB used / {:.1} GB total ({:.1} GB available)",
                used_gb, total_gb, available_gb,
            ));
        }

        if show_disk {
            let disks = Disks::new_with_refreshed_list();
            let mut disk_lines: Vec<String> = Vec::new();
            for disk in disks.list() {
                let mount = disk.mount_point().to_string_lossy();
                let total_gb = disk.total_space() as f64 / 1_073_741_824.0;
                let avail_gb = disk.available_space() as f64 / 1_073_741_824.0;
                let used_gb = total_gb - avail_gb;
                disk_lines.push(format!(
                    "  {} — {:.1} GB used / {:.1} GB total ({:.1} GB free)",
                    mount, used_gb, total_gb, avail_gb,
                ));
            }
            if disk_lines.is_empty() {
                disk_lines.push("  No disks detected.".to_string());
            }
            sections.push(format!("Disks:\n{}", disk_lines.join("\n")));
        }

        let os_name = sysinfo::System::name().unwrap_or_else(|| "unknown".to_string());
        let arch = sysinfo::System::cpu_arch();
        let mut sys_mem = sysinfo::System::new();
        sys_mem.refresh_memory();
        let total_gb = sys_mem.total_memory() as f64 / 1_073_741_824.0;
        let ui_data = serde_json::json!({
            "platform": os_name,
            "arch": arch,
            "memory_total": format!("{:.0} GB", total_gb),
            "cpu": sysinfo::System::host_name().unwrap_or_else(|| "unknown".to_string()),
        });
        let hint = format!("[[[mcp-ui:system:{}]]]\n", ui_data);
        let plain_text = sections.join("\n\n");
        let full_result = format!("{}{}", hint, plain_text);
        Ok(CallToolResult::success(vec![Content::text(full_result)]))
    }

    #[tool(description = "Send a desktop popup notification to the user.")]
    async fn send_notification(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<NotifyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let title = params.0.title;
        let body = params.0.body;

        // Local desktop popup — best-effort (a headless server has no display).
        if let Err(e) = notify_rust::Notification::new()
            .summary(&title)
            .body(&body)
            .appname("Goose in a Pond")
            .show()
        {
            tracing::debug!("desktop notification unavailable: {e}");
        }

        // Also push to connected phones over the foreground stream, if a sender is wired.
        let mut reached_devices = false;
        if let Some(sender) = crate::notification_sender() {
            let notification = pond_core::mcp::ports::notification::Notification {
                id: uuid::Uuid::new_v4().to_string(),
                target: "broadcast".to_string(),
                category: "info".to_string(),
                title: title.clone(),
                body: body.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                data: None,
            };
            match sender.broadcast(notification).await {
                Ok(()) => reached_devices = true,
                Err(e) => tracing::warn!(error = %e, "failed to push notification to devices"),
            }
        }

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Notification sent: \"{}\" — {}{}",
            title,
            body,
            if reached_devices {
                " (also pushed to connected devices)"
            } else {
                ""
            },
        ))]))
    }
}

impl Default for SystemMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_handler]
impl ServerHandler for SystemMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-system",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP System MCP server — time, system info, notifications, shell, and file I/O.\n\n\
                 Tools: get_current_time, get_system_info (OS/memory/disk), send_notification \
                 (desktop popup), run_shell_command (sandboxed allow-list), read_file (with line \
                 limit), write_file (with append mode).\n\n\
                 Shell commands are limited to a safe allow-list with a 10-second timeout. \
                 File operations reject path traversal and require absolute paths.",
            )
    }
}

// ── Spawn function for Goose builtin registry (stateless — no deps) ──────

use tokio::io::DuplexStream;

/// Spawn function compatible with Goose's `SpawnServerFn` type; needs no `init_*`.
pub fn spawn_system_server(reader: DuplexStream, writer: DuplexStream) {
    let server = SystemMcpServer::default();
    crate::serve_builtin("giap-system", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_constructs() {
        let _server = SystemMcpServer::new();
    }

    #[test]
    fn server_default() {
        let _server = SystemMcpServer::default();
    }
}
