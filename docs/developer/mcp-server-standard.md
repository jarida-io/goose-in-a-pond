# GIAP MCP Server Design Standard

How GIAP's own tools are built. Each `giap-*` extension is an `rmcp` server in
`crates/pond-mcp-server`, registered with Goose as a builtin and served in-process over a duplex
pipe. For third-party servers see [Building Extensions](extensions.md).

## The servers

| Extension | Module | Tools | Enabled by |
|---|---|---|---|
| `giap-toolkit` | `toolkit.rs` | `list_tool_groups`, `enable_tool_group` | always |
| `giap-memory` | `memory.rs` | `recall_memories`, `save_memory`, `forget_memory` | `ext_memory_enabled` |
| `giap-system` | `system.rs` | `get_current_time`, `get_system_info`, `send_notification` | `ext_system_enabled` |
| `giap-schedule` | `schedule.rs` | `list_schedules`, `set_timer`, `create_schedule`, `schedule_action`, `world_clock` | `ext_schedule_enabled` (and a scheduler) |
| `giap-weather` | `weather.rs` | `get_current_weather`, `get_weather_forecast` | `ext_weather_enabled` |
| `giap-knowledge` | `knowledge.rs`, `web_search.rs`, `web_page.rs`, `wolfram.rs` | `get_wikipedia_article`, `search_web`, `read_web_page`, `compute_answer` | `ext_knowledge_enabled` |
| `giap-device` | `device.rs` | `list_registered_devices`, `set_audio_device`, `get_user_profile`, `list_skills`, `load_skill` | `ext_device_enabled` |
| `giap-device-control` | `device_control.rs` | `describe_device`, `get_device_state`, `set_device_state` | `ext_device_enabled` |
| `giap-sensors` | `sensors.rs` | `get_sensor_reading`, `get_sensor_history`, `list_sensors` | `ext_sensor_enabled` |
| `giap-travel` | `travel.rs` | `get_directions_link`, `book_ride`, `get_ride_link` | `ext_travel_enabled` (off by default) |
| `giap-context` | `context.rs` | `search_context`, `recall`, `get_recent_context` | `ext_context_enabled` (off by default) |
| `giap-orchestrator` | `orchestrator.rs` | `delegate`, `check_task` | `ext_orchestrator_enabled` (off by default) |

38 tools. Registration is in `register_giap_extensions`
(`crates/pond-adapters-goose/src/giap_registration.rs`). `GIAP_NO_TOOLS` registers none.
`search_web` and `compute_answer` are offered only while their keys (`BRAVE_SEARCH_API_KEY`,
`WOLFRAM_APP_ID`) are stored (`TOOL_REQUIREMENTS` in `crates/pond-core/src/mcp/domain/tool_group.rs`).

## Principles

1. **Small models pick the right tool and send the wrong parameters.** Design for `{}`: every
   parameter optional, with a fallback chain.
2. **Context is scarce.** Every schema is in the prompt every turn it is offered, and the tool block
   is the start of the KV prefix. Keep descriptions and results short.
3. **One domain per server**, a handful of tools, holding only the ports it needs.
4. **Fail in words the model can act on.** Never panic, never return `ErrorData` for a failure the
   user caused or the world caused.

## Descriptions

The `#[tool(description = ...)]` string and the doc comments on parameter fields (which become the
JSON Schema descriptions) are what the model reads. Changing either changes the prompt.

- Short, and say when to use the tool and what not to do instead:

```rust
#[tool(description = "\
Current weather. Omit location for the configured home rather than asking \
which city. Never guess weather or shell out for it.")]
```

- A tool group's catalogue entry (`TOOL_GROUPS`) is embedded and scored against what the user says,
  so phrase it like a request ("Reminders, alarms, timers..."), not an API summary.

## Parameters

Every field `Option`, plus a catch-all for the shapes models actually send:

```rust
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WeatherParams {
    pub location: Option<String>,
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}
```

Resolve in this order: the named parameter, then likely keys in `extra` (`location`, `city`,
`place`, ...), then, where it makes sense, the user's last message (`crate::last_user_message()`).

`get_wikipedia_article` and `create_schedule` also ask an optional argument specialist first:
`crate::generate_params(tool_name, schema)` calls the `ToolCaller` set from the `tool_model` setting
(a small GGUF run by `ToolCallerEngine` in `pond-adapters-local-inference`). With no `tool_model`, it
returns `None` and the chain above applies.

## Results

Budgets are bytes, not characters (non-Latin script costs more tokens per character). Use the
helpers in `crates/pond-mcp-server/src/format.rs`:

| Helper | For |
|---|---|
| `truncate_to_budget(text, max_bytes)` | Cut on a char boundary with a notice |
| `format_list_result(items, header, max_bytes)` | A capped list; empty becomes a miss |
| `format_no_results(what, alternatives)` | A miss that names fully-qualified tools to try next |
| `format_dead_end(what, why)` | A miss where trying another tool would be wrong |
| `format_api_error(service, error)` | An upstream failure, steering away from a retry |
| `format_not_configured(feature, signup_url)` | A tool that cannot work without a key |
| `format_degraded` / `degrade_result` | A keyless fallback's answer, with a note appended |

Budgets in use: Wikipedia extracts 4,000 bytes, `search_web` 1,800 (2,400 wide), `read_web_page`
1,800 of quoted lines or one 3,200-byte part, `compute_answer` 1,800.

A result that a desktop card renders starts with a `[[[mcp-ui:<kind>:<json>]]]` line (weather,
knowledge). The weather card is also an MCP App resource, `ui://giap-weather/weather-card.html`
(`crates/pond-mcp-server/apps/`), listed by `all_app_resources()`.

## Failures

- **A miss or an upstream error**: `CallToolResult::success` with text from the helpers above, so the
  model chains to another tool or answers.
- **A failure no other tool can fix** (weather with no location set): `CallToolResult::error` with
  the reason in plain words.
- **`ErrorData`** only for protocol errors. The model reports it as a technical fault.

## Egress

Every outbound request goes through the egress gate, so an `offline` pond refuses it and the request
is recorded. In this crate, use `crate::http::traced_get` / `traced_get_with`; an adapter crate uses
its own gated sender (`pond-adapters-weather`'s `traced_send`). Call `crate::set_current_tool(name)`
at the top of a tool so its egress is attributed.

Keys come from the secret store via `crate::secret(KEY)`, which returns `None` when unset so the tool
can fall back.

## Structure

```rust
#[derive(Clone)]
pub struct WeatherMcpServer {
    weather: Option<Arc<dyn WeatherProvider>>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl WeatherMcpServer {
    /// Every tool this server exposes, without constructing it.
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new(weather: Option<Arc<dyn WeatherProvider>>) -> Self {
        Self { weather, tool_router: Self::tool_router() }
    }

    #[tool(description = "...")]
    async fn get_current_weather(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WeatherParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_current_weather");
        // ...
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}

#[tool_handler]
impl ServerHandler for WeatherMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new("giap-weather", env!("CARGO_PKG_VERSION")))
            .with_instructions("What this server does and how to use it.")
    }
}
```

A tool that needs the calling conversation reads it from the request's `_meta`
(`session_from_meta`, key `agent-session-id`), never from a model-filled parameter.

### Dependencies and spawn

Goose's spawn function is a plain `fn(DuplexStream, DuplexStream)`, so dependencies sit in a
`OnceLock`. The spawn function must not panic: a panic during extension loading takes down every
builtin's startup.

```rust
static WEATHER_DEPS: OnceLock<WeatherDeps> = OnceLock::new();

pub fn init_weather_deps(weather: Option<Arc<dyn WeatherProvider>>) {
    let _ = WEATHER_DEPS.set(WeatherDeps { weather });
}

pub fn spawn_weather_server(reader: DuplexStream, writer: DuplexStream) {
    let Some(deps) = WEATHER_DEPS.get() else {
        tracing::error!("spawn_weather_server called before init_weather_deps — extension will not start");
        return;
    };
    let server = WeatherMcpServer::new(deps.weather.clone());
    crate::serve_builtin("giap-weather", server, reader, writer);
}
```

`serve_builtin` logs `kind="mcp_server_stopped"` if the server ends; a builtin is not restarted.

## Adding a server

1. Module in `crates/pond-mcp-server/src/`, with `tool_defs()`, `init_*_deps`, `spawn_*_server`;
   re-export from `lib.rs`.
2. A `ToolGroup` in `TOOL_GROUPS` (`crates/pond-core/src/mcp/domain/tool_group.rs`).
   `crates/pond-core/tests/registration_matches_the_catalog.rs` fails if the registration list and
   the catalogue disagree; an uncatalogued builtin is treated as a user-added server and never
   narrowed.
3. Register it in `register_giap_extensions` behind a new `ext_<domain>_enabled` setting. A new
   setting needs both an `upsert!` in `update_fields` and an arm in `apply_key`
   (`crates/pond-infra/src/sqlite_settings.rs`, guarded by `roundtrip_persists_every_field`), and a
   disposition (`every_settings_field_is_dispositioned` in `pond-core`).
4. Add it to `prefix_oracle::all_tools`. `the_default_tool_prefix_is_unchanged` pins the default tool
   block; update the pin deliberately.
5. A key-gated tool goes in `TOOL_REQUIREMENTS`.
6. Unit tests in the module: parameter resolution from direct params and from `extra`, construction
   with no dependencies. Network tests are `#[ignore]`d.
7. `cargo clippy -p pond-mcp-server` and `cargo test -p pond-mcp-server -p pond-core` (both are in
   CI's fast crates).
