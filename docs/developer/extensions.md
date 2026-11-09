# Building Extensions for GIAP

An extension is an MCP (Model Context Protocol) server. Once it is added, its tools are offered to
the assistant like GIAP's own. GIAP's tools are themselves MCP servers, compiled into the server
binary (see [GIAP MCP Server Design Standard](mcp-server-standard.md)); this guide is for servers
you write or install yourself.

## How extensions are loaded

```
pond-server
  GooseAdapter (Goose agent)
    Goose extension manager
      builtin   giap-* servers in this process (crates/pond-mcp-server)
      stdio     your server as a child process
      streamable_http   your server over HTTP
```

- **Builtins** are registered at startup by `register_giap_extensions`
  (`crates/pond-adapters-goose/src/giap_registration.rs`), each behind an `ext_*_enabled` setting,
  plus `giap-toolkit`, which is always on.
- **Added extensions** are saved in `pond_system.db` (`SqliteMcpServerRepository`) and started again
  on every server start. A saved extension that is switched off, or whose required key is not
  stored, is not started.
- **Catalogue extensions** come from `crates/pond-core/src/extensions/marketplace_registry.json`,
  compiled in (`BundledMarketplace`). Installing one saves it like an added extension; its declared
  keys go to the secret store, not the saved row.

Tool names reach the model as `<extension>__<tool>`, for example `giap-weather__get_current_weather`
or `my-ext__search_docs`. Goose keys extensions by a normalised name (`name_to_key`: lower case,
characters other than letters, digits, `-` and `_` become `_`), so give an extension a name that is
already in that form.

Which tools a turn is offered also depends on `tool_selection_mode` (Settings > Extensions >
Tool loading in the hub): `all` (default), `relevant`, or `minimal`. Only `giap-*` groups listed in
`TOOL_GROUPS` (`crates/pond-core/src/mcp/domain/tool_group.rs`) are narrowed; an added extension's
tools are always offered.

In `relevant` mode each group's description is embedded and scored against the opening message,
the household's memories and its skills, each as a separate signal, keeping a group's best score.
A message that lists several things ("a briefing on weather, calendar, top news and the exchange
rate", the usual shape of a scheduled brief) also has each item scored on its own, up to eight:
the whole message's vector sits between its topics, so a topic named in one word would otherwise
fall under the floor and the model would answer it from the web
(`selection_signals` in `crates/pond-core/src/mcp/services/tool_selection.rs`).

### The extension session

Added extensions belong to one engine session named `giap-extensions`, not to a chat; chats see their
tools through the shared Goose agent. In `resolve_extension_session`
(`crates/pond-adapters-goose/src/extension_manager.rs`):

- **Its `working_dir` is where a stdio extension is spawned.** It is re-pinned to the server's
  current working directory each time the session is resolved.
- **It is found by name, not by id.** Deleting a chat only deletes engine sessions named after a chat
  UUID, so no user action removes it. If the row is missing, it is recreated and the operation
  proceeds.

Log lines: `bound the extension manager to a session` names the session id in use;
`the cached extension session is gone — re-resolving` records a recovery.

## Writing an extension

Start from a template in [`templates/extensions/`](../../templates/extensions/):

| Template | Runtime | Notes |
|---|---|---|
| [`python/`](../../templates/extensions/python/) | Python 3.10+, standard library only | Synchronous line loop over stdin |
| [`typescript/`](../../templates/extensions/typescript/) | Node with `tsx` | `readline` over stdin |
| [`rust/`](../../templates/extensions/rust/) | `serde` + `serde_json` only | Synchronous, standalone Cargo project |

Each speaks MCP over stdio as newline-delimited JSON-RPC and has two tools, `greet` and `timestamp`.
For anything beyond a demo, an MCP SDK (the official Python or TypeScript SDK, or `rmcp` in Rust)
handles the protocol for you. [`extensions/music/`](../../extensions/music/) is a full extension
GIAP ships, written in TypeScript.

### Methods to handle

| Method | Purpose |
|--------|---------|
| `initialize` | Handshake: return `protocolVersion`, `capabilities`, `serverInfo`, and optionally `instructions` |
| `notifications/initialized` | Notification; send no response |
| `tools/list` | Return the tools |
| `tools/call` | Run a tool and return content blocks |

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"my-ext","version":"0.1.0"}}}
```

### Tools

Each tool has a `name`, a `description` and an `inputSchema` (JSON Schema). The description is what
the model uses to decide when to call the tool, so say when to use it, not only what it does.
Parameter `description`s matter as much.

```json
{
  "name": "search_docs",
  "description": "Search the project documentation. Use for questions about installing or configuring the project.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "query": {"type": "string", "description": "What to search for"},
      "max_results": {"type": "integer", "description": "At most this many results (default 5)"}
    },
    "required": ["query"]
  }
}
```

A tool call returns content blocks:

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_docs","arguments":{"query":"install"}}}
{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"1. Getting Started (page 2)"}]}}
```

### Rules that matter here

- **Write logs to stderr.** Stdout carries only protocol messages.
- **Stay alive.** Answer a failure with a JSON-RPC error (`-32601` unknown method or tool,
  `-32602` invalid params, `-32603` internal) or with `"isError": true` content the model can read.
  A process that exits is not restarted (see [Recovering](#recovering-a-stopped-extension)).
- **Keep results small.** On a local model, Goose caps a tool result at a quarter of the context
  window (`GOOSE_MAX_TOOL_RESPONSE_SIZE`, set in `goose_env_knobs`); the rest is cut.
- **Start fast.** GIAP waits for `initialize` before the extension's tools appear.
- **Explain having no tools.** A server that deliberately lists no tools on this machine should say
  why in the first sentence of its `initialize` `instructions`; the dashboard shows it.
- **Time out your own network calls.**

### Testing by hand

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"greet","arguments":{"name":"Jerry"}}}' \
  | python3 server.py
```

## Adding an extension

### In the desktop app

Either Extensions surface works: **Extensions** (the Add Extension button; the Browse tab for the
catalogue) or the hub's **Settings > Extensions** (Add server; Browse).

### Over the API

All routes below are protected: they answer only after onboarding, and need a bearer token from a
paired device. For local development, start the server with `POND_DEV_ALLOW_LOOPBACK=1` and requests
from this machine skip the token. The bound port is in `<data_dir>/.runtime_api_port` (4000 unless
taken).

```bash
API=http://127.0.0.1:$(cat "$HOME/Library/Application Support/goose-in-a-pond/.runtime_api_port")/api/v1

curl -X POST "$API/extensions" -H "Content-Type: application/json" -d '{
  "name": "my-ext",
  "kind": "stdio",
  "command": "python3",
  "args": ["/absolute/path/to/server.py"],
  "description": "My extension"
}'
```

| Field | Kinds | Notes |
|---|---|---|
| `name` | all | Becomes the tool prefix |
| `kind` | | `stdio`, `streamable_http` (or `http`), `builtin` |
| `command` | stdio | Checked with `which` before starting; must be on the server's `PATH` |
| `args` | stdio | Relative paths resolve against the extension session's `working_dir`; use absolute paths |
| `env` | stdio | Saved in the extension's row in `pond_system.db`. Goose drops variables such as `PATH` and `LD_PRELOAD` (`Envs::DISALLOWED_KEYS`) |
| `uri` | streamable_http | Probed with a GET first; the probe goes through the egress gate, so an `offline` pond refuses it |

For an API key, prefer a catalogue entry with `required_secrets` (below): its values live in the
secret store and are read into the environment at each start.

## Catalogue entries

An entry in `marketplace_registry.json`:

```json
{
  "id": "github",
  "name": "GitHub",
  "description": "Manage repositories, issues, pull requests, and branches on GitHub",
  "kind": "stdio",
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-github"],
  "category": "development",
  "author": "Anthropic",
  "tools": ["create_issue", "list_issues", "create_pull_request", "search_repositories"],
  "featured": true,
  "required_secrets": [
    {"key": "GITHUB_PERSONAL_ACCESS_TOKEN", "display_name": "GitHub Token",
     "description": "Create at github.com/settings/tokens with repo scope",
     "required": true, "kind": "api_key"}
  ]
}
```

- `required_secrets[].kind`: `api_key`, `oauth_flow` (GIAP runs the sign-in), `generic`, or
  `choice` (one of `options`; not secret, so its value is read back). `host_only` keeps a key out of
  the extension's environment; `advanced` puts it under developer settings.
- A required key that is pasted (not a sign-in) blocks install and start until it is stored.
- Args beginning `extensions/` are anchored at the server's asset root, so a bundled extension
  (`music`) starts from any working directory.
- A catalogue install also passes `GIAP_INTERNAL_TOKEN` (minted again on every server start) and
  `GIAP_SERVER_URL`, for calling back into the pond.

Current entries: `filesystem`, `github`, `brave-search`, `memory`, `puppeteer`, `slack`, `music`.

## Status

`GET /extensions` reports each extension's `status`:

| `status` | Meaning | Dashboard |
|----------|---------|-----------|
| `connected` | Running, with tools | Green dot |
| `no_tools` | Running, listing no tools on purpose (the music extension on a pond that is not a Mac). `status_reason` is the first sentence of its `initialize` `instructions`, at most 200 characters, or `null` | Grey dot, "Running, no assistant tools here", and the reason |
| `error` | Failed to start. `last_error` is the engine's error from this run, or "Extension failed to load" for a saved, enabled extension the engine has no record of | Red dot and the error |
| `disabled` | Saved and switched off | Grey dot |

Each connect of a tool-less extension logs one info line with `kind="extension_without_tools"`.

### Recovering a stopped extension

A stdio process that exits is not restarted. Switch it off and on (`PATCH` with `false`, then
`true`), which starts it again from its stored config, or restart the server. Storing its keys with
`POST /extensions/{name}/secrets` also restarts it.

## API reference

Base path `/api/v1`. All are protected routes (see above).

| Method and path | Body | Success | Errors |
|---|---|---|---|
| `GET /extensions` | | `{"extensions": [ExtensionInfo]}` | 500, 503 no extension manager |
| `POST /extensions` | `AddExtensionRequest` | 201 `ExtensionInfo` | 400 (command not on `PATH`, URI unreachable, unknown kind, start failed), 503 |
| `DELETE /extensions/{name}` | | 204 | 404, 503 |
| `PATCH /extensions/{name}` | `{"enabled": bool}` | `{"name", "enabled"}` | 400 bad JSON, 500, 503 |
| `GET /extensions/{name}/secrets` | | `{"requirements", "fulfilled", "values"}` | 404 not in the catalogue, 503 |
| `POST /extensions/{name}/secrets` | flat `{"KEY": "value"}` | `{"stored", "restarted", "restart_error"}` | 400, 404, 500, 503 |
| `GET /marketplace` | | `{"extensions": [MarketplaceExtension]}` | 500, 503 |
| `POST /marketplace/{id}/install` | optional `{"secrets": {...}}` | 201 `ExtensionInfo` | 400, 404, 428 missing secrets, 500, 503 |
| `GET /agent/tools` | | `[{"extension", "name", "description", "_meta"?}]` | 500 |
| `GET /dev/goose` | | `{"goose_active", "extension_count", "extensions", "tool_count", "tools"}` | |

`ExtensionInfo` (`crates/pond-core/src/mcp/ports/extension_manager.rs`):

```json
{
  "name": "giap-weather",
  "kind": "builtin",
  "description": "",
  "tools": ["get_current_weather", "get_weather_forecast"],
  "enabled": true,
  "status": "connected",
  "last_error": null,
  "status_reason": null
}
```

`tools` holds names without the extension prefix. For a running extension, `kind` reads `builtin`
and `description` is empty whatever its transport: the extension manager does not track either. A
saved extension that is not running is reported from its saved row instead.

`POST /extensions/{name}/secrets` stores the keys before it restarts anything, so read all three
fields:

| `restarted` | `restart_error` | Meaning |
|---|---|---|
| `true` | `null` | Stored, and running with them |
| `false` | `null` | Stored; not installed or switched off, so nothing was restarted |
| `false` | a string | Stored, but not running with them; the string says why |

`POST /marketplace/{id}/install` answers 428 with
`{"error": "Missing required secrets", "missing": [{"key", "display_name", "description", "kind"}]}`
when a blocking key is neither in the body nor already stored.

`GET /agent/tools` adds `_meta.ui.resourceUri` to tools with an MCP App card (today
`giap-weather__get_current_weather` -> `ui://giap-weather/weather-card.html`), read through
`GET /mcp/resources?uri=...`.
