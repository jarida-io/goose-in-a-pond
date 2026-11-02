# MCP Specification Compliance

How GIAP uses the [Model Context Protocol](https://modelcontextprotocol.io/specification): protocol
versions, transports, primitives, the built-in tool set, extension registration and credentials.
Goose (the `goose/` submodule) is the MCP **client**; GIAP's own capabilities are MCP **servers**
in `crates/pond-mcp-server`.

---

## Protocol versions

| Component | Version | Where |
|---|---|---|
| `rmcp` crate | `=2.1.0` | root `Cargo.toml` `[workspace.dependencies]` and `goose/Cargo.toml`; one copy in `Cargo.lock` |
| Versions `rmcp` 2.1.0 knows | `2024-11-05`, `2025-03-26`, `2025-06-18`, `2025-11-25` (its `LATEST`), `2026-07-28` | `rmcp::model::ProtocolVersion` |
| Goose MCP client | requests `2025-03-26` | `GooseClient::get_info` in `goose/crates/goose/src/agents/mcp_client.rs` |
| GIAP built-in servers | `get_info` states `2024-11-05` | every `ServerHandler` in `crates/pond-mcp-server/src/` |
| Music extension | always answers `2024-11-05` | `extensions/music/src/server.ts` (`initialize`) |

`rmcp`'s server side echoes the client's requested version when it is one it knows
(`negotiate_protocol_version`) and falls back to the server's stated version otherwise. So a Goose
session with a built-in server runs at `2025-03-26`; `2024-11-05` is only the fallback.

---

## Transports

Goose's `ExtensionConfig` (`goose/crates/goose/src/agents/extension.rs`) has these variants:

| Variant | GIAP use |
|---|---|
| `builtin` | GIAP's servers: registered with `register_builtin_extension` and run in-process over a tokio `DuplexStream` pair (`spawn_*_server`) |
| `stdio` | marketplace extensions and user-added commands; the child process speaks JSON-RPC on stdin/stdout |
| `streamable_http` | user-added remote servers (`kind` `http` or `streamable_http`) |
| `sse` | config-file compatibility only ("SSE transport is no longer supported") |
| `platform`, `frontend`, `inline_python` | Goose extension types; GIAP's REST API does not create them |

---

## Primitives

| Primitive | Goose client | GIAP servers |
|---|---|---|
| Tools | `list_tools` (with cursor), `call_tool` | the only capability advertised (`ServerCapabilities::builder().enable_tools()`) |
| Resources | `list_resources`, `read_resource` | not exposed over MCP; the weather card `ui://giap-weather/weather-card.html` is served over REST as a `ReadResourceResult` by `GET /api/v1/mcp/resources?uri=`, and the card calls tools through `POST /api/v1/mcp/tools/call` |
| Prompts | `list_prompts`, `get_prompt` | not exposed |
| Client capabilities | `roots`, `sampling` (`create_message`), `elicitation`, plus Goose's MCP-UI extension | not used by GIAP's servers |

---

## Built-in tools (37)

Registered by `register_giap_extensions` (`crates/pond-adapters-goose/src/giap_registration.rs`)
from `Settings.ext_*_enabled` at startup. Each `#[tool(...)]` method below is one tool; the model
sees it as `<extension>__<tool>`.

| Extension | Module(s) | Default | Tools |
|---|---|---|---|
| `giap-toolkit` | `toolkit.rs` | always on | `list_tool_groups`, `enable_tool_group` |
| `giap-memory` | `memory.rs` | on | `recall_memories`, `save_memory`, `forget_memory` |
| `giap-schedule` | `schedule.rs` | on (needs a scheduler) | `list_schedules`, `set_timer`, `create_schedule`, `schedule_action`, `world_clock` |
| `giap-weather` | `weather.rs` | on | `get_current_weather`, `get_weather_forecast` |
| `giap-knowledge` | `knowledge.rs`, `wolfram.rs`, `web_search.rs`, `web_page.rs` | on | `get_wikipedia_article`, `compute_answer`, `search_web`, `read_web_page` |
| `giap-system` | `system.rs` | on | `get_current_time`, `get_system_info`, `send_notification` |
| `giap-device` | `device.rs` | on (`ext_device_enabled`) | `list_registered_devices`, `set_audio_device`, `get_user_profile`, `list_skills`, `load_skill` |
| `giap-device-control` | `device_control.rs` | with `giap-device` | `describe_device`, `get_device_state`, `set_device_state` |
| `giap-sensors` | `sensors.rs` | on | `get_sensor_reading`, `get_sensor_history`, `list_sensors` |
| `giap-travel` | `travel.rs` | off | `get_directions_link`, `book_ride`, `get_ride_link` |
| `giap-context` | `context.rs` | off | `search_context`, `recall`, `get_recent_context` |
| `giap-orchestrator` | `orchestrator.rs` | off | `delegate`, `check_task` |

Modules without tools: `audio.rs` (device-name resolution behind `set_audio_device`), `secrets.rs`
(key lookup and per-turn withholding), `session_meta.rs` (the GIAP session id from a request's
`_meta`), `dispatcher.rs`, `format.rs`, `http.rs`, `prefix_oracle.rs`. `format.rs` holds a test that
parses `#[tool(` out of the server sources, so a plain text search for the attribute counts two
extra matches there.

Per turn, `GooseAdapter` withholds a tool whose key is not stored
(`tools_withheld_for_missing_secrets`): `search_web` needs `BRAVE_SEARCH_API_KEY`, `compute_answer`
needs `WOLFRAM_APP_ID` (`tool_group::TOOL_REQUIREMENTS`). `GIAP_NO_TOOLS` registers no built-in
servers at all; `GiapProviderShim` (`provider_shim.rs`) vetoes, at `Provider::stream`, any tool
outside the turn's allow-set.

---

## Extension registration

| Path | Route / entry point | What happens |
|---|---|---|
| Built-in | `register_giap_extensions` at startup | Goose builtin registry; loaded once per session |
| User-added | `POST /api/v1/extensions` (`AddExtensionRequest`: `name`, `kind`, `description`, `command`, `args`, `env`, `uri`) | `GiapGooseExtensionManager::add_extension` (`crates/pond-adapters-goose/src/extension_manager.rs`): a stdio `command` must resolve with `which`; an HTTP `uri` is probed (5 s) after an egress check (`EgressPurpose::Extensions`); the config is added to Goose on the `giap-extensions` engine session; tools are polled three times at 500 ms; status is `connected` or `no_tools`. The handler saves the row to `mcp_servers` and registers the tools. |
| Catalogue | `GET /api/v1/marketplace`, `POST /api/v1/marketplace/{id}/install` | `BundledMarketplace` (`crates/pond-core/src/mcp/services/marketplace.rs`) parses `crates/pond-core/src/extensions/marketplace_registry.json` (`include_str!`). Missing blocking secrets answer 428; supplied secrets go to the secret store; the start environment gets `GIAP_INTERNAL_TOKEN` and `GIAP_SERVER_URL`; the saved row keeps neither the declared secrets nor the token. |
| Restart | `pond-server` startup | Every `mcp_servers` row is re-added unless disabled. Catalogue rows go through `extension_env::settle_saved_env` first; a row missing a blocking secret is not started. A fresh per-process `GIAP_INTERNAL_TOKEN` is injected, and relative `extensions/...` args are re-anchored to the asset root (`anchor_asset_args`). |
| Manage | `DELETE` / `PATCH /api/v1/extensions/{name}`, `GET`/`POST /api/v1/extensions/{name}/secrets` | remove, toggle, and read or set an extension's keys |

Catalogue entries (registry `version` 1; the parser reads only `extensions`):

| id | Command | Secrets |
|---|---|---|
| `filesystem` | `npx -y @modelcontextprotocol/server-filesystem /` | none |
| `github` | `npx -y @modelcontextprotocol/server-github` | `GITHUB_PERSONAL_ACCESS_TOKEN` |
| `brave-search` | `npx -y @modelcontextprotocol/server-brave-search` | `BRAVE_API_KEY` |
| `memory` | `npx -y @modelcontextprotocol/server-memory` | none |
| `puppeteer` | `npx -y @modelcontextprotocol/server-puppeteer` | none |
| `slack` | `npx -y @modelcontextprotocol/server-slack` | `SLACK_BOT_TOKEN` |
| `music` | `npx -y tsx extensions/music/src/server.ts` | `MUSIC_SERVICE`, `MUSIC_PLAYER`, `SPOTIFY_CLIENT_ID`, `SPOTIFY_ACCESS_TOKEN`, `APPLE_MUSIC_TEAM_ID`, `APPLE_MUSIC_KEY_ID`, `APPLE_MUSIC_PRIVATE_KEY` |

### The music extension

`extensions/music` is GIAP's own stdio extension: a TypeScript JSON-RPC loop over stdin/stdout
(no MCP SDK) run with `tsx`, advertising tools only. Its tools are `play`, `playlists`, `library`,
`devices`, `status` and `control` (`src/tools.ts`). It plays Apple Music; Spotify is played by hand
(see [`extensions/music/README.md`](../../extensions/music/README.md) and
[Music player](./music-player.md)). It reaches the pond through `GIAP_SERVER_URL` with
`GIAP_INTERNAL_TOKEN`. CI covers it in the `music-extension` job.

---

## Authorization and credentials

### Extension environment

Goose's `Envs::new` drops 31 keys from any extension's environment, with a warning
(`Envs::DISALLOWED_KEYS` in `goose/crates/goose/src/agents/extension.rs`):

| Category | Keys |
|---|---|
| Binary path (4) | `PATH`, `PATHEXT`, `SystemRoot`, `windir` |
| Linux dynamic linker (6) | `LD_LIBRARY_PATH`, `LD_PRELOAD`, `LD_AUDIT`, `LD_DEBUG`, `LD_BIND_NOW`, `LD_ASSUME_KERNEL` |
| macOS dynamic linker (3) | `DYLD_LIBRARY_PATH`, `DYLD_INSERT_LIBRARIES`, `DYLD_FRAMEWORK_PATH` |
| Language runtimes (9) | `PYTHONPATH`, `PYTHONHOME`, `NODE_OPTIONS`, `RUBYOPT`, `GEM_PATH`, `GEM_HOME`, `CLASSPATH`, `GO111MODULE`, `GOROOT` |
| Windows process / DLL (7) | `APPINIT_DLLS`, `SESSIONNAME`, `ComSpec`, `TEMP`, `TMP`, `LOCALAPPDATA`, `USERPROFILE` |
| Windows home (2) | `HOMEDRIVE`, `HOMEPATH` |

Every GIAP path builds stdio configs through `Envs::new`, so this applies to user-added,
catalogue and restored extensions alike. GIAP adds no list of its own.

`crates/pond-core/src/mcp/services/extension_env.rs` decides what a catalogue extension starts with:
key values live in the secret store, a saved row gives up its copy once the store holds the key,
each start reads the store (so a deleted key stops working everywhere), and host-only secrets never
enter the environment.

### HTTP extensions

GIAP's add path builds `StreamableHttp` configs with no custom headers. Goose handles
authentication: with stored credentials it refreshes and connects; on an auth failure it runs
`oauth_flow` (`goose/crates/goose/src/oauth/`), which uses rmcp's `AuthorizationManager`, serves a
loopback callback at `/oauth_callback` on a random port and opens the browser with `webbrowser`.

### Where credentials live

| Credential | Store |
|---|---|
| Extension keys, OAuth tokens from GIAP's own `/api/v1/oauth/*` PKCE routes (provider: `spotify`), web-search and Wolfram keys | `FileSecretRepository` (`crates/pond-infra/src/file_secret_repository.rs`): `<data_dir>/secrets.json`, XChaCha20-Poly1305 with a key file (`secret_crypto.rs`); environment variables override it |
| Goose-side MCP OAuth credentials | `GooseCredentialStore` -> Goose `Config::set_secret`. GIAP builds Goose without its `system-keyring` feature, so these go to Goose's file store, `secrets.yaml` (mode 0600, plain YAML) in Goose's config dir, which `pond-server` pins to `<data_dir>/engine/config/` through `GOOSE_PATH_ROOT` |

### REST access

The extension, marketplace, secrets and `oauth/authorize` routes require the API token.
`GET /oauth/callback` and `POST /oauth/refresh` are public routes; the refresh handler checks
`GIAP_INTERNAL_TOKEN` (`crates/pond-api/src/middleware/mod.rs`, `PUBLIC_ROUTES`).

---

## Key dependencies

| Crate | Version | Role |
|---|---|---|
| `rmcp` | `=2.1.0` | MCP types, client/server traits, stdio and streamable-HTTP transports, `auth` |
| `schemars` | `1.0.2` | JSON Schema for tool parameters |
| `oauth2` | `5.0` | OAuth primitives in Goose |
| `chacha20poly1305` | `0.10` | GIAP secret-store encryption |
| Goose | submodule `goose/` (workspace version `1.44.0` in `goose/Cargo.toml`) | MCP client, extension manager, OAuth flow |

---

## References

- [MCP specification](https://modelcontextprotocol.io/specification)
- [rmcp crate](https://crates.io/crates/rmcp)
- [Goose patch management](../goose-patch-management.md): the GIAP patch set on the Goose fork
- [Extension developer guide](../developer/extensions.md): building MCP servers for GIAP
