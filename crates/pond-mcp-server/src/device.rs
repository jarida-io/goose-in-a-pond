//! Device MCP server: registered devices, user profile and user skills.

use pond_core::models::domain::audio_device::{
    AudioDevice, AudioDeviceStatus, AudioDirection, ChoiceBasis,
};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::guest_room_access::GuestRoomAccess;
use pond_core::user_data::ports::room_registry::RoomRegistry;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::skill::UserSkillRepository;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ErrorCode, ErrorData, Implementation, InitializeResult,
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
pub struct AudioDeviceParams {
    /// "speaker" or "microphone"
    pub which: String,
    /// Its name, or "automatic"; omit to list
    pub device: Option<String>,
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
    room_registry: Arc<dyn RoomRegistry + Send + Sync>,
    guest_room_access: Arc<dyn GuestRoomAccess + Send + Sync>,
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
        room_registry: Arc<dyn RoomRegistry + Send + Sync>,
        guest_room_access: Arc<dyn GuestRoomAccess + Send + Sync>,
        settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
        skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
    ) -> Self {
        Self {
            device_registry,
            room_registry,
            guest_room_access,
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
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.device_registry.list_devices().await {
            Ok(devices) => {
                // A guest only ever sees what it may also control; a device with no room is
                // off-limits by default since there's nothing to check it against.
                let scope = ctx
                    .extensions
                    .get::<ProfileScope>()
                    .cloned()
                    .unwrap_or(ProfileScope::Household);
                let devices = if matches!(scope, ProfileScope::Guest) {
                    let accessible = self
                        .guest_room_access
                        .accessible_room_ids()
                        .await
                        .unwrap_or_default();
                    devices
                        .into_iter()
                        .filter(|d| {
                            d.room_id
                                .as_deref()
                                .is_some_and(|id| accessible.contains(id))
                        })
                        .collect::<Vec<_>>()
                } else {
                    devices
                };

                let text = if devices.is_empty() {
                    if matches!(scope, ProfileScope::Guest) {
                        "No devices are available to a guest here.".to_string()
                    } else {
                        "No devices registered.".to_string()
                    }
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
                    // Resolved once up front: the map below is a sync closure over a value only
                    // an awaited call can produce.
                    let room_names: std::collections::HashMap<String, String> = self
                        .room_registry
                        .list_rooms()
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|r| (r.id, r.name))
                        .collect();
                    let ui_devices: Vec<serde_json::Value> = devices
                        .iter()
                        .map(|d| {
                            let room = d
                                .room_id
                                .as_deref()
                                .and_then(|id| room_names.get(id))
                                .cloned()
                                .unwrap_or_default();
                            serde_json::json!({
                                "name": d.name,
                                "is_online": d.is_online,
                                "device_type": d.device_type,
                                "room": room,
                            })
                        })
                        .collect();
                    let ui_data = serde_json::json!({ "devices": ui_devices });
                    let hint = format!("[[[mcp-ui:devices:{}]]]\n", ui_data);
                    let full_result = format!("{}{}", hint, text);
                    return Ok(CallToolResult::success(vec![ContentBlock::text(
                        full_result,
                    )]));
                }

                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Error listing devices: {}", e),
                None,
            )),
        }
    }

    #[tool(
        description = "Switch the speaker or microphone ('use the USB speaker'), or back to \
                       automatic. No device: lists them."
    )]
    async fn set_audio_device(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<AudioDeviceParams>,
    ) -> Result<CallToolResult, ErrorData> {
        use crate::audio::{self, Pick};
        let text = |t: String| Ok(CallToolResult::success(vec![ContentBlock::text(t)]));
        let Some(control) = audio::audio_control() else {
            return text("This pond is not choosing its own speaker and microphone here.".into());
        };
        let Some(direction) = audio::direction_of(&params.0.which) else {
            return text("Say which: \"speaker\" or \"microphone\".".into());
        };
        let blocking = |f: Box<dyn FnOnce() -> AudioDeviceStatus + Send>| async move {
            tokio::task::spawn_blocking(f).await.map_err(|e| {
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("audio devices: {e}"),
                    None,
                )
            })
        };
        let c = control.clone();
        let before = blocking(Box::new(move || c.status())).await?;
        let choice = match direction {
            AudioDirection::Input => &before.input,
            AudioDirection::Output => &before.output,
        };
        let said = params.0.device.as_deref().map(str::trim).unwrap_or("");
        if said.is_empty() {
            return text(audio::listing(choice));
        }
        if choice.basis == ChoiceBasis::MicOff {
            return text("The microphone is off in Settings, so there is none to switch.".into());
        }
        let known: Vec<AudioDevice> = choice.devices.iter().map(|p| p.device.clone()).collect();
        let (pin, asked) = match audio::resolve(&known, said) {
            Pick::Automatic => (String::new(), None),
            Pick::One(d) => (d.id.clone(), Some(d)),
            Pick::Several(labels) => {
                return text(format!("Which one: {}?", labels.join(", ")));
            }
            Pick::Nothing => {
                return text(format!(
                    "No {} called '{said}'. {}",
                    direction.noun(),
                    audio::listing(choice)
                ));
            }
        };
        let key = audio::pin_key(direction);
        let saved = async {
            self.settings_repo.set_key(key, pin).await?;
            self.settings_repo
                .mark_user_set(&std::iter::once(key.to_string()).collect())
                .await
        };
        if let Err(e) = saved.await {
            return Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("could not save the {}: {e}", direction.noun()),
                None,
            ));
        }
        // A running voice session chooses again on its own thread and may play its test sound
        // first, so its answer is waited for.
        let after = blocking(Box::new(move || {
            control.reselect();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
            loop {
                let now = control.status();
                if now.updated_at_ms > before.updated_at_ms || std::time::Instant::now() >= deadline
                {
                    return now;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }))
        .await?;
        let choice = match direction {
            AudioDirection::Input => &after.input,
            AudioDirection::Output => &after.output,
        };
        text(audio::outcome(choice, asked.as_ref()))
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
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
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
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
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
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    crate::format::format_dead_end(
                        &format!("an active skill named '{}'", params.0.name),
                        &hint,
                    ),
                )]))
            }
            Some(s) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
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
                 get_recipe (YAML agent recipe by name), set_audio_device (switch the \
                 speaker or microphone, or list them).",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct DeviceDeps {
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    room_registry: Arc<dyn RoomRegistry + Send + Sync>,
    guest_room_access: Arc<dyn GuestRoomAccess + Send + Sync>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
}

static DEVICE_DEPS: OnceLock<DeviceDeps> = OnceLock::new();

/// Initialize device server dependencies. Call once at startup.
pub fn init_device_deps(
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    room_registry: Arc<dyn RoomRegistry + Send + Sync>,
    guest_room_access: Arc<dyn GuestRoomAccess + Send + Sync>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
) {
    let _ = DEVICE_DEPS.set(DeviceDeps {
        device_registry,
        room_registry,
        guest_room_access,
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
        deps.room_registry.clone(),
        deps.guest_room_access.clone(),
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

    use pond_core::user_data::mocks::mock_room_registry::MockRoomRegistry;

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
            Arc::new(MockRoomRegistry),
            Arc::new(pond_core::user_data::mocks::mock_guest_room_access::MockGuestRoomAccess),
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
            Arc::new(MockRoomRegistry),
            Arc::new(pond_core::user_data::mocks::mock_guest_room_access::MockGuestRoomAccess),
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

    /// Records the pin the tool saves, so the fake control can choose it as the pond would.
    #[derive(Default)]
    struct PinSettings {
        set: std::sync::Mutex<Vec<(String, String)>>,
        marked: std::sync::Mutex<Vec<String>>,
    }
    #[async_trait]
    impl SettingsRepository for PinSettings {
        async fn get(&self) -> anyhow::Result<Settings> {
            Ok(Settings::default())
        }
        async fn update(&self, _: &Settings) -> anyhow::Result<()> {
            Ok(())
        }
        async fn get_key(&self, _: &str) -> anyhow::Result<Option<String>> {
            Ok(None)
        }
        async fn set_key(&self, key: &str, value: String) -> anyhow::Result<()> {
            self.set.lock().unwrap().push((key.to_string(), value));
            Ok(())
        }
        async fn mark_user_set(
            &self,
            keys: &std::collections::HashSet<String>,
        ) -> anyhow::Result<()> {
            self.marked.lock().unwrap().extend(keys.iter().cloned());
            Ok(())
        }
    }

    struct FakeAudio {
        settings: Arc<PinSettings>,
        status: std::sync::Mutex<AudioDeviceStatus>,
    }
    impl pond_core::models::ports::audio_devices::AudioDeviceControl for FakeAudio {
        fn status(&self) -> AudioDeviceStatus {
            self.status.lock().unwrap().clone()
        }
        fn retest(&self, _: bool) -> AudioDeviceStatus {
            self.status()
        }
        fn reselect(&self) {
            use pond_core::models::domain::audio_device::{choose, ProbeOutcome, ProbedDevice};
            let pin = self
                .settings
                .set
                .lock()
                .unwrap()
                .last()
                .map(|(_, v)| v.clone());
            let mut status = self.status.lock().unwrap();
            let probed: Vec<ProbedDevice> = status
                .output
                .devices
                .iter()
                .map(|p| ProbedDevice::new(p.device.clone(), ProbeOutcome::Accepting))
                .collect();
            status.output = choose(AudioDirection::Output, probed, pin.as_deref(), false);
            status.updated_at_ms += 1;
        }
    }

    #[tokio::test]
    async fn asking_for_a_speaker_by_name_pins_it_and_reports_the_switch() {
        use pond_core::models::domain::audio_device::{
            choose, AudioChoice, DeviceKind, ProbeOutcome, ProbedDevice,
        };
        let settings = Arc::new(PinSettings::default());
        let mut usb = AudioDevice::new("sysdefault:CARD=Device", false, DeviceKind::Hardware);
        usb.label = "USB PnP Audio Device".into();
        let speakers = vec![
            ProbedDevice::new(
                AudioDevice::new("Built-in speakers", true, DeviceKind::Hardware),
                ProbeOutcome::Accepting,
            ),
            ProbedDevice::new(usb, ProbeOutcome::Untested),
        ];
        let status = AudioDeviceStatus {
            input: AudioChoice::mic_off(None),
            output: choose(AudioDirection::Output, speakers, None, false),
            updated_at_ms: 1,
            source: "serve".into(),
            pid: 1,
            raised: Vec::new(),
            sound_server: None,
        };
        crate::audio::init_audio_control(Arc::new(FakeAudio {
            settings: settings.clone(),
            status: std::sync::Mutex::new(status),
        }));
        let server = DeviceMcpServer::new(
            Arc::new(StubDeviceRegistry),
            Arc::new(MockRoomRegistry),
            Arc::new(pond_core::user_data::mocks::mock_guest_room_access::MockGuestRoomAccess),
            settings.clone(),
            Arc::new(StubSkills),
        );
        let ask = |which: &str, device: Option<&str>| {
            let server = server.clone();
            let params = AudioDeviceParams {
                which: which.into(),
                device: device.map(str::to_string),
            };
            async move {
                let ctx = make_ctx(server.clone()).await;
                tool_text(
                    &server
                        .set_audio_device(ctx, Parameters(params))
                        .await
                        .unwrap(),
                )
            }
        };

        let listed = ask("speaker", None).await;
        assert_eq!(
            listed,
            "Speakers: Built-in speakers (in use), USB PnP Audio Device. Chosen automatically."
        );
        let switched = ask("speaker", Some("the USB speaker")).await;
        assert!(
            switched.starts_with("The speaker is now USB PnP Audio Device (pinned"),
            "{switched}"
        );
        assert_eq!(
            *settings.set.lock().unwrap(),
            [(
                "audio_output_device".to_string(),
                "sysdefault:CARD=Device".to_string()
            )]
        );
        assert_eq!(*settings.marked.lock().unwrap(), ["audio_output_device"]);
        let unknown = ask("speaker", Some("the headphones")).await;
        assert!(
            unknown.starts_with("No speaker called 'the headphones'."),
            "{unknown}"
        );
        let off = ask("microphone", Some("USB")).await;
        assert!(off.contains("off in Settings"), "{off}");
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

    struct OneDeviceInKitchen;
    #[async_trait]
    impl DeviceRegistry for OneDeviceInKitchen {
        async fn register(&self, _: RegisterDeviceRequest) -> anyhow::Result<Device> {
            unimplemented!()
        }
        async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
            Ok(vec![Device {
                id: "d1".into(),
                name: "Lamp".into(),
                device_type: "light".into(),
                hostname: None,
                ip_address: None,
                capabilities: vec![],
                registered_at: String::new(),
                last_seen: None,
                is_online: true,
                room_id: Some("room-1".into()),
            }])
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

    struct OneRoomNamedKitchen;
    #[async_trait]
    impl RoomRegistry for OneRoomNamedKitchen {
        async fn create(
            &self,
            _: &str,
        ) -> anyhow::Result<pond_core::user_data::ports::room_registry::Room> {
            unimplemented!()
        }
        async fn list_rooms(
            &self,
        ) -> anyhow::Result<Vec<pond_core::user_data::ports::room_registry::Room>> {
            Ok(vec![pond_core::user_data::ports::room_registry::Room {
                id: "room-1".into(),
                name: "Kitchen".into(),
                created_at: String::new(),
            }])
        }
        async fn get_room(
            &self,
            _: &str,
        ) -> anyhow::Result<Option<pond_core::user_data::ports::room_registry::Room>> {
            Ok(None)
        }
        async fn rename(&self, _: &str, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn list_registered_devices_resolves_room_id_to_name() {
        let server = DeviceMcpServer::new(
            Arc::new(OneDeviceInKitchen),
            Arc::new(OneRoomNamedKitchen),
            Arc::new(pond_core::user_data::mocks::mock_guest_room_access::MockGuestRoomAccess),
            Arc::new(StubSettings),
            Arc::new(StubSkills),
        );
        let ctx = make_ctx(server.clone()).await;
        let result = server.list_registered_devices(ctx).await.unwrap();
        let text = tool_text(&result);
        assert!(
            text.contains("\"room\":\"Kitchen\""),
            "expected a resolved room name in the hint JSON, got: {text}"
        );
    }
}
