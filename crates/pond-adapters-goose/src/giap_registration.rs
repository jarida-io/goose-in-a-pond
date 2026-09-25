//! Registers GIAP's MCP servers in Goose's builtin extension registry.

use anyhow::Result;
use goose::builtin_extension::register_builtin_extension;
use pond_adapters_weather::WeatherProvider;
use pond_core::mcp::ports::tools::tool_caller::ToolCaller;
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::device_control::DeviceControlPort;
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::scheduler::SchedulerPort;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::skill::UserSkillRepository;
use std::sync::{Arc, OnceLock};

/// Set once by `register_giap_extensions()`; GooseAdapter filters extensions by it.
static REGISTERED_EXTENSIONS: OnceLock<Vec<String>> = OnceLock::new();

/// Extensions registered at startup; empty before `register_giap_extensions()` runs.
pub fn registered_extensions() -> &'static [String] {
    REGISTERED_EXTENSIONS
        .get()
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

/// Register the MCP servers enabled by `ext_*_enabled` (plus the always-on toolkit) and return
/// their names. Call once at startup, before any GooseAdapter session.
pub fn register_giap_extensions(
    settings: &Settings,
    memory_repo: Arc<dyn MemoryRepository + Send + Sync>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    scheduler: Option<Arc<dyn SchedulerPort>>,
    weather: Option<Arc<dyn WeatherProvider>>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
    device_control: Arc<dyn DeviceControlPort + Send + Sync>,
    tool_caller: Option<Arc<dyn ToolCaller>>,
) -> Result<Vec<String>> {
    // `GIAP_NO_TOOLS`: spawn no servers. Only a saving; the provider shim is what guarantees no
    // tools, since Goose's platform extensions and user-added servers bypass this.
    if pond_core::mcp::domain::tool_group::no_tools_env_set() {
        tracing::warn!(
            "{} is set — registering no extensions at all. Unset it to restore normal behaviour.",
            pond_core::mcp::domain::tool_group::NO_TOOLS_ENV
        );
        println!("  Extensions: NONE (GIAP_NO_TOOLS is set)");
        return Ok(Vec::new());
    }

    // Set the ToolCaller specialist — all MCP tools use it for param generation
    pond_mcp_server::set_tool_caller(tool_caller);

    let mut registered = Vec::new();

    // ── Always-on: toolkit server (escape hatch) ────────────────────────────
    // Not toggleable: under `"minimal"` its two tools are the entire tool surface.
    // Its handle comes later from `init_toolkit_deps`; without it both tools report all loaded.
    register_builtin_extension(
        pond_core::mcp::domain::tool_group::TOOLKIT_EXTENSION,
        pond_mcp_server::spawn_toolkit_server,
    );
    registered.push(pond_core::mcp::domain::tool_group::TOOLKIT_EXTENSION.into());

    // ── Toggleable extensions ───────────────────────────────────────────────

    if settings.ext_memory_enabled {
        pond_mcp_server::init_memory_deps(memory_repo, embedding_provider);
        register_builtin_extension("giap-memory", pond_mcp_server::spawn_memory_server);
        registered.push("giap-memory".into());
    }

    if settings.ext_schedule_enabled {
        if let Some(sched) = scheduler {
            pond_mcp_server::init_schedule_deps(sched, settings_repo.clone());
            register_builtin_extension("giap-schedule", pond_mcp_server::spawn_schedule_server);
            registered.push("giap-schedule".into());
        }
    }

    if settings.ext_weather_enabled {
        pond_mcp_server::init_weather_deps(weather);
        register_builtin_extension("giap-weather", pond_mcp_server::spawn_weather_server);
        registered.push("giap-weather".into());
    }

    if settings.ext_knowledge_enabled {
        pond_mcp_server::init_knowledge_deps(reqwest::Client::new());
        register_builtin_extension("giap-knowledge", pond_mcp_server::spawn_knowledge_server);
        registered.push("giap-knowledge".into());
    }

    if settings.ext_system_enabled {
        register_builtin_extension("giap-system", pond_mcp_server::spawn_system_server);
        registered.push("giap-system".into());
    }

    if settings.ext_device_enabled {
        pond_mcp_server::init_device_deps(
            device_registry.clone(),
            settings_repo.clone(),
            skill_repo,
        );
        register_builtin_extension("giap-device", pond_mcp_server::spawn_device_server);
        registered.push("giap-device".into());

        // The registry resolves natural references ("the light") to device ids.
        pond_mcp_server::init_device_control_deps(device_control, device_registry);
        register_builtin_extension(
            "giap-device-control",
            pond_mcp_server::spawn_device_control_server,
        );
        registered.push("giap-device-control".into());
    }

    // Storage handle comes from `init_sensor_deps` in pond-server, where the logs DB lives.
    if settings.ext_sensor_enabled {
        register_builtin_extension("giap-sensors", pond_mcp_server::spawn_sensor_server);
        registered.push("giap-sensors".into());
    }

    // Personal context: read-only, scoped to the speaker via the session id in `_meta`. Never add
    // an `ingest_context` tool: injected text could be planted and later quoted as fact.
    if settings.ext_context_enabled {
        register_builtin_extension(
            "giap-context",
            pond_mcp_server::context::spawn_context_server,
        );
        registered.push("giap-context".into());
    }

    // Orchestration: OFF by default (`default_ext_orchestrator_enabled`), so an upgrade never
    // silently grants an autonomous `GooseMode::Auto` agent.
    if settings.ext_orchestrator_enabled {
        register_builtin_extension(
            pond_core::mcp::domain::tool_group::ORCHESTRATOR_EXTENSION,
            pond_mcp_server::spawn_orchestrator_server,
        );
        registered.push(pond_core::mcp::domain::tool_group::ORCHESTRATOR_EXTENSION.into());
    }

    let _ = REGISTERED_EXTENSIONS.set(registered.clone());

    tracing::info!(
        extensions = ?registered,
        "Registered {} builtin MCP modules", registered.len()
    );

    Ok(registered)
}
