//! Device MCP server: registered devices, user profile and user skills.

use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::skill::UserSkillRepository;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorCode, ErrorData, Implementation, InitializeResult,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ── Parameter structs ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetRecipeParams {
    /// Recipe name (slug).
    pub name: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct LoadSkillParams {
    /// Skill name, exactly as shown by `list_skills`.
    pub name: String,
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DeviceMcpServer {
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl DeviceMcpServer {
    /// Every tool this server exposes, without constructing it; `tool_router()` is module-private.
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new(
        device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
        settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
        skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
    ) -> Self {
        Self {
            device_registry,
            settings_repo,
            skill_repo,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "List registered devices, online status, and what each can be told to \
                       do. For exact accepted values (modes, limits, sensor units): \
                       describe_device."
    )]
    async fn list_registered_devices(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.device_registry.list_devices().await {
            Ok(devices) => {
                let text = if devices.is_empty() {
                    "No devices registered.".to_string()
                } else {
                    devices
                        .iter()
                        .map(|d| {
                            // Capabilities spare the model trial-and-error in front of the user.
                            let can = if d.capabilities.is_empty() {
                                String::new()
                            } else {
                                format!(" — {}", d.capabilities.join(", "))
                            };
                            format!(
                                "- {} ({}): {}{can}",
                                d.name,
                                d.device_type,
                                if d.is_online { "online" } else { "offline" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };

                if !devices.is_empty() {
                    let ui_devices: Vec<serde_json::Value> = devices
                        .iter()
                        .map(|d| {
                            serde_json::json!({
                                "name": d.name,
                                "is_online": d.is_online,
                                "device_type": d.device_type,
                                "room": d.room.clone().unwrap_or_default(),
                            })
                        })
                        .collect();
                    let ui_data = serde_json::json!({ "devices": ui_devices });
                    let hint = format!("[[[mcp-ui:devices:{}]]]\n", ui_data);
                    let full_result = format!("{}{}", hint, text);
                    return Ok(CallToolResult::success(vec![Content::text(full_result)]));
                }

                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Error listing devices: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Get user profile: name, assistant name, timezone, location.")]
    async fn get_user_profile(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let settings = self.settings_repo.get().await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Settings error: {}", e),
                None,
            )
        })?;
        // The location service falls back to the time zone, so this is usually a place.
        let location = pond_core::user_data::services::location::resolve(&settings)
            .describe()
            .unwrap_or("unknown")
            .to_string();
        let text = format!(
            "User: {}\nAssistant name: {}\nTimezone: {}\nLocation: {}",
            settings.user_name, settings.assistant_name, settings.timezone, location,
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "List active user skills by name and description. Call load_skill to get \
                        a skill's full instructions."
    )]
    async fn list_skills(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let skills = self.skill_repo.list_active().await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Skills error: {}", e),
                None,
            )
        })?;
        let text = if skills.is_empty() {
            "No active skills.".to_string()
        } else {
            skills
                .iter()
                .map(|s| format!("- {}: {}", s.name, s.description))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Load a user skill's full instructions into context by name.")]
    async fn load_skill(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<LoadSkillParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let skill = self
            .skill_repo
            .get_by_name(&params.0.name)
            .await
            .map_err(|e| {
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Skills error: {}", e),
                    None,
                )
            })?;
        match skill {
            None => {
                let active = self.skill_repo.list_active().await.unwrap_or_default();
                let requested = params.0.name.to_lowercase();
                let suggestions: Vec<&str> = active
                    .iter()
                    .filter(|s| {
                        s.name.to_lowercase().contains(&requested)
                            || requested.contains(&s.name.to_lowercase())
                    })
                    .take(3)
                    .map(|s| s.name.as_str())
                    .collect();
                let hint = if suggestions.is_empty() {
                    "Call list_skills to see the active skills.".to_string()
                } else {
                    format!("Did you mean: {}?", suggestions.join(", "))
                };
                Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_dead_end(
                        &format!("an active skill named '{}'", params.0.name),
                        &hint,
                    ),
                )]))
            }
            Some(s) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Skill: {}\n{}\n\n{}",
                s.name, s.description, s.content
            ))])),
        }
    }
}

#[tool_handler]
impl ServerHandler for DeviceMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-device",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Device MCP server — device registry, user profile, model configuration, \
                 skills, and agent recipes.\n\n\
                 Tools: list_registered_devices, get_user_profile (name/timezone/location), \
                 get_model_assignments (active LLM config), list_skills (active user skill \
                 names + descriptions), load_skill (a skill's full instructions by name — call \
                 this when a listed skill looks relevant before acting on it), \
                 get_recipe (YAML agent recipe by name).",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct DeviceDeps {
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
}

static DEVICE_DEPS: OnceLock<DeviceDeps> = OnceLock::new();

/// Initialize device server dependencies. Call once at startup.
pub fn init_device_deps(
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
) {
    let _ = DEVICE_DEPS.set(DeviceDeps {
        device_registry,
        settings_repo,
        skill_repo,
    });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_device_server(reader: DuplexStream, writer: DuplexStream) {
    // Not every binary installs these deps; a panic here would take down every builtin server.
    let Some(deps) = DEVICE_DEPS.get() else {
        tracing::error!(
            "spawn_device_server called before init_device_deps — extension will not start"
        );
        return;
    };
    let server = DeviceMcpServer::new(
        deps.device_registry.clone(),
        deps.settings_repo.clone(),
        deps.skill_repo.clone(),
    );
    crate::serve_builtin("giap-device", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pond_core::user_data::domain::settings::Settings;
    use pond_core::user_data::domain::skill::UserSkill;
    use pond_core::user_data::ports::device_registry::{Device, RegisterDeviceRequest};

    struct StubDeviceRegistry;
    #[async_trait]
    impl DeviceRegistry for StubDeviceRegistry {
        async fn register(&self, _: RegisterDeviceRequest) -> anyhow::Result<Device> {
            unimplemented!()
        }
        async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
            Ok(vec![])
        }
        async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> {
            Ok(None)
        }
        async fn unregister(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct StubSettings;
    #[async_trait]
    impl SettingsRepository for StubSettings {
        async fn get(&self) -> anyhow::Result<Settings> {
            Ok(Settings::default())
        }
        async fn update(&self, _: &Settings) -> anyhow::Result<()> {
            Ok(())
        }
        async fn get_key(&self, _: &str) -> anyhow::Result<Option<String>> {
            Ok(None)
        }
        async fn set_key(&self, _: &str, _: String) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct StubSkills;
    #[async_trait]
    impl UserSkillRepository for StubSkills {
        async fn list_active(&self) -> anyhow::Result<Vec<UserSkill>> {
            Ok(vec![])
        }
        async fn list_all(&self) -> anyhow::Result<Vec<UserSkill>> {
            Ok(vec![])
        }
        async fn get(&self, _: &str) -> anyhow::Result<Option<UserSkill>> {
            Ok(None)
        }
        async fn get_by_name(&self, _: &str) -> anyhow::Result<Option<UserSkill>> {
            Ok(None)
        }
        async fn create(&self, _: &UserSkill) -> anyhow::Result<()> {
            Ok(())
        }
        async fn update(&self, _: &UserSkill) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_server() -> DeviceMcpServer {
        DeviceMcpServer::new(
            Arc::new(StubDeviceRegistry),
            Arc::new(StubSettings),
            Arc::new(StubSkills),
        )
    }

    #[test]
    fn server_constructs() {
        let _server = test_server();
    }

    struct StubSkillsWithData;
    #[async_trait]
    impl UserSkillRepository for StubSkillsWithData {
        async fn list_active(&self) -> anyhow::Result<Vec<UserSkill>> {
            Ok(vec![UserSkill {
                id: "1".to_string(),
                name: "morning-briefing".to_string(),
                description: "Summarizes the day each morning.".to_string(),
                icon: "sparkles".to_string(),
                content: "Read the calendar and weather, then summarize.".to_string(),
                active: true,
                created_at: "2026-01-01T00:00:00Z".to_string(),
            }])
        }
        async fn list_all(&self) -> anyhow::Result<Vec<UserSkill>> {
            self.list_active().await
        }
        async fn get(&self, _: &str) -> anyhow::Result<Option<UserSkill>> {
            Ok(None)
        }
        async fn get_by_name(&self, name: &str) -> anyhow::Result<Option<UserSkill>> {
            Ok(self
                .list_active()
                .await?
                .into_iter()
                .find(|s| s.name == name))
        }
        async fn create(&self, _: &UserSkill) -> anyhow::Result<()> {
            Ok(())
        }
        async fn update(&self, _: &UserSkill) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_server_with_skill() -> DeviceMcpServer {
        DeviceMcpServer::new(
            Arc::new(StubDeviceRegistry),
            Arc::new(StubSettings),
            Arc::new(StubSkillsWithData),
        )
    }

    async fn make_ctx(server: DeviceMcpServer) -> RequestContext<RoleServer> {
        use rmcp::model::RequestId;
        use rmcp::service::serve_directly;

        let (_client, stream) = tokio::io::duplex(64);
        let running = serve_directly(server, stream, None);
        RequestContext::new(RequestId::Number(0), running.peer().clone())
    }

    fn tool_text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn load_skill_returns_full_content() {
        let server = test_server_with_skill();
        let ctx = make_ctx(server.clone()).await;
        let result = server
            .load_skill(
                ctx,
                Parameters(LoadSkillParams {
                    name: "morning-briefing".to_string(),
                }),
            )
            .await
            .unwrap();
        let text = tool_text(&result);
        assert!(text.contains("Summarizes the day each morning."));
        assert!(text.contains("Read the calendar and weather, then summarize."));
    }

    #[tokio::test]
    async fn load_skill_missing_returns_dead_end_with_suggestion() {
        let server = test_server_with_skill();
        let ctx = make_ctx(server.clone()).await;
        let result = server
            .load_skill(
                ctx,
                Parameters(LoadSkillParams {
                    name: "morning-brief".to_string(),
                }),
            )
            .await
            .unwrap();
        let text = tool_text(&result);
        assert!(text.contains("Did you mean: morning-briefing?"));
    }

    #[tokio::test]
    async fn list_skills_includes_description() {
        let server = test_server_with_skill();
        let ctx = make_ctx(server.clone()).await;
        let result = server.list_skills(ctx).await.unwrap();
        let text = tool_text(&result);
        assert!(text.contains("morning-briefing: Summarizes the day each morning."));
    }
}
