# Building Extensions for GIAP

Extensions add new capabilities to your GIAP agent using the Model Context Protocol (MCP). When you add an extension, its tools become available to the agent during conversations -- the agent can discover and call them just like the built-in GIAP tools.

## What is an MCP Extension?

An MCP extension is a program that implements the Model Context Protocol -- a JSON-RPC interface over stdio or HTTP. GIAP starts the extension as a subprocess (stdio) or connects to it over HTTP (streamable HTTP), then makes its tools available to the Goose agent.

The protocol is standardized: any MCP-compatible server works with GIAP. You can use existing MCP servers from the ecosystem or build your own.

## Quick Start

1. Pick a template from `templates/extensions/` (Python, TypeScript, or Rust)
2. Copy it to your project directory
3. Add your tools
4. Register it with GIAP

## Architecture

```
GIAP Server
  |-- Goose Agent
       |-- MCP Extension Manager
            |-- stdio --> your extension (child process)
            |-- streamable_http --> your extension (network)
            |-- builtin --> giap tools (in-process)
```

When GIAP starts, it loads all persisted extension configs and connects to each one. The Goose agent sees tools from all extensions in a unified namespace, prefixed by extension name (e.g. `my-ext__my_tool`).

### The extension session

Extensions are not owned by a chat. They are added to and removed from one dedicated engine session named `giap-extensions`, and chat sessions inherit the tools from it. Two properties of that session matter when debugging an extension that will not start:

- **Its `working_dir` is the cwd your stdio extension is spawned in.** It is re-pinned to the server's current working directory every time the session is resolved, so it does not go stale across restarts launched from different directories.
- **It is found by name, not by id.** The name is the identity, which is what makes the row safe: deleting a chat cascades to that chat's engine session, and only rows named after a chat's UUID are ever deleted, so no user action can remove the extension session. If the row is missing when an extension operation runs — deleted by hand, or the engine store was wiped — it is recreated on the spot and the operation proceeds.

Both behaviours live in `resolve_extension_session` (`crates/pond-adapters-goose/src/extension_manager.rs`). A log line reading `bound the extension manager to a session` records which session id is in use, and `the cached extension session is gone — re-resolving` records a recovery.

## Creating an Extension

### Step 1: Implement the MCP Protocol

Your extension must handle these JSON-RPC methods over stdin/stdout (one JSON object per line):

| Method | Direction | Purpose |
|--------|-----------|---------|
| `initialize` | GIAP --> ext | Handshake -- return your server info and capabilities |
| `notifications/initialized` | GIAP --> ext | Notification that init is complete (no response needed) |
| `tools/list` | GIAP --> ext | Return the list of tools your extension provides |
| `tools/call` | GIAP --> ext | Execute a tool and return the result |

**Example: initialize request and response**

```json
// Request (from GIAP)
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}

// Response (from your extension)
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"my-ext","version":"0.1.0"}}}
```

### Step 2: Define Your Tools

Each tool needs:
- `name` -- unique identifier within your extension (e.g. `"search_docs"`)
- `description` -- what the tool does (the agent reads this to decide when to use it, so be clear and specific)
- `inputSchema` -- JSON Schema defining the tool's parameters

The description is critical: it is the primary signal the agent uses to decide whether to invoke your tool. Write it as if explaining the tool to a colleague.

**Example: tools/list response**

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "tools": [
      {
        "name": "search_docs",
        "description": "Search the project documentation for relevant articles. Returns matching excerpts with page references.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "query": {
              "type": "string",
              "description": "The search query"
            },
            "max_results": {
              "type": "integer",
              "description": "Maximum number of results to return (default: 5)"
            }
          },
          "required": ["query"]
        }
      }
    ]
  }
}
```

### Step 3: Handle Tool Calls

When the agent decides to use your tool, GIAP sends a `tools/call` request. Return results as an array of content blocks:

```json
// Request
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_docs","arguments":{"query":"installation","max_results":3}}}

// Response
{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"Found 3 results:\n1. Getting Started (page 2)\n2. ..."}]}}
```

Content block types:
- `{"type": "text", "text": "..."}` -- plain text (most common)
- `{"type": "image", "data": "<base64>", "mimeType": "image/png"}` -- images

### Step 4: Register with GIAP

**Via REST API:**

```bash
curl -X POST http://localhost:4000/api/v1/extensions \
  -H "Content-Type: application/json" \
  -d '{
    "name": "my-extension",
    "kind": "stdio",
    "command": "python",
    "args": ["path/to/server.py"],
    "description": "My custom extension"
  }'
```

**Via Desktop App:**

Open the Extensions section in the GIAP desktop app and click "Add Extension".

**Via Marketplace:**

If your extension is published in the curated registry, users can install it with one click from the Browse tab.

Registration is persisted -- your extension auto-reconnects when GIAP restarts.

## Extension Types

### Stdio (recommended for local extensions)

GIAP starts your extension as a subprocess and communicates via stdin/stdout. This is the simplest approach and works for any language.

```json
{
  "name": "my-ext",
  "kind": "stdio",
  "command": "python",
  "args": ["server.py"]
}
```

The `command` is resolved against `PATH`. Use absolute paths if needed. The working directory is inherited from the GIAP server process.

### Streamable HTTP (for remote/shared extensions)

GIAP connects to your extension over HTTP using MCP's streamable HTTP transport. Use this when the extension runs on a different machine or is shared across multiple GIAP instances.

```json
{
  "name": "my-ext",
  "kind": "streamable_http",
  "uri": "http://localhost:3000/mcp"
}
```

## Environment Variables

Pass secrets and configuration via the `env` field:

```json
{
  "name": "github",
  "kind": "stdio",
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-github"],
  "env": {
    "GITHUB_TOKEN": "ghp_..."
  }
}
```

Environment variables are injected into the extension's process and are NOT shared with other extensions. Use this for API keys, database URLs, and other configuration that should not be hardcoded.

## Testing Your Extension

### Manual test with stdin/stdout

Run your extension and pipe JSON-RPC messages to it:

```bash
# Initialize
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | python server.py

# List tools
echo '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' | python server.py

# Call a tool
echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"greet","arguments":{"name":"Jerry"}}}' | python server.py
```

Each command starts a fresh process. For multi-message testing, use a script or interactive session that keeps stdin open.

### Integration test with GIAP

1. Start GIAP: `cargo run -p pond-server -- serve`
2. Register your extension via the REST API
3. Verify it loaded: `GET /api/v1/extensions` should list your extension and its tools
4. Send a chat message that should trigger your tool (e.g. "greet Jerry")
5. Check the response includes your tool's output

### Verify tool discovery

```bash
# List all extensions and their tools
curl http://localhost:4000/api/v1/extensions

# List all available MCP tools across all extensions
curl http://localhost:4000/api/v1/dev/goose
```

## Error Handling

Return JSON-RPC errors for failures. Do not crash or exit -- GIAP expects the process to stay alive.

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "error": {
    "code": -32601,
    "message": "Unknown tool: nonexistent_tool"
  }
}
```

Standard error codes:
- `-32700` -- Parse error (malformed JSON)
- `-32600` -- Invalid request
- `-32601` -- Method/tool not found
- `-32602` -- Invalid params
- `-32603` -- Internal error

If your extension crashes, GIAP logs the failure but does not automatically restart it. The user will need to re-enable or re-add the extension.

## Best Practices

- **Keep tools focused** -- one tool per action, clear descriptions. A tool that does five things is harder for the agent to use correctly than five tools that each do one thing.
- **Handle errors gracefully** -- return JSON-RPC error responses, do not crash. The agent can recover from tool errors but not from a dead process.
- **Document parameters** -- the agent uses `description` fields on both tools and their parameters to understand how to call them. Be specific: "The GitHub username (e.g. 'octocat')" is better than "The user".
- **Timeout protection** -- if your tool does network I/O, add timeouts. GIAP has its own timeout but your tool should handle its own failures.
- **Stateless when possible** -- the agent may call tools in any order and may not call them at all. Do not rely on tools being called in a specific sequence.
- **Minimize startup time** -- for stdio extensions, GIAP waits for the `initialize` response before proceeding. Fast startup improves the user experience.
- **Log to stderr** -- stdout is reserved for MCP protocol messages. Write debug logs to stderr.

## Managing Extensions

### List all extensions

```bash
curl http://localhost:4000/api/v1/extensions
```

Response:
```json
{
  "extensions": [
    {
      "name": "giap",
      "kind": "builtin",
      "tools": ["giap__get_current_weather", "giap__recall_memories", "..."]
    },
    {
      "name": "my-ext",
      "kind": "stdio",
      "tools": ["my-ext__search_docs"]
    }
  ]
}
```

### Add an extension

```bash
curl -X POST http://localhost:4000/api/v1/extensions \
  -H "Content-Type: application/json" \
  -d '{"name": "my-ext", "kind": "stdio", "command": "python", "args": ["server.py"]}'
```

### Remove an extension

```bash
curl -X DELETE http://localhost:4000/api/v1/extensions/my-ext
```

### Enable/disable an extension

Toggle without removing the persisted config:

```bash
curl -X PATCH http://localhost:4000/api/v1/extensions/my-ext \
  -H "Content-Type: application/json" \
  -d '{"enabled": false}'
```

### Browse the marketplace

```bash
curl http://localhost:4000/api/v1/marketplace
```

### Install from marketplace

```bash
curl -X POST http://localhost:4000/api/v1/marketplace/weather/install
```

## Templates

Ready-to-use starter templates with working examples:

- [Python template](../../templates/extensions/python/) -- zero dependencies, raw MCP protocol over asyncio
- [TypeScript template](../../templates/extensions/typescript/) -- Node.js with tsx, readline-based
- [Rust template](../../templates/extensions/rust/) -- serde + serde_json only, synchronous I/O, fast startup

Each template includes two example tools (`greet` and `timestamp`) and a README with setup instructions.

## Using Existing MCP Servers

The MCP ecosystem has many existing servers you can use directly with GIAP. Any server that speaks the MCP protocol works. Examples:

```bash
# GitHub (official MCP server)
curl -X POST http://localhost:4000/api/v1/extensions \
  -H "Content-Type: application/json" \
  -d '{
    "name": "github",
    "kind": "stdio",
    "command": "npx",
    "args": ["-y", "@modelcontextprotocol/server-github"],
    "env": {"GITHUB_TOKEN": "ghp_..."}
  }'

# Filesystem access
curl -X POST http://localhost:4000/api/v1/extensions \
  -H "Content-Type: application/json" \
  -d '{
    "name": "filesystem",
    "kind": "stdio",
    "command": "npx",
    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/documents"]
  }'
```

See [modelcontextprotocol.io](https://modelcontextprotocol.io) for the full directory of available servers.

---

## API Reference

All extension endpoints require authentication (Bearer token) and return JSON. Base URL: `http://localhost:4000/api/v1`.

### Extension Endpoints

#### `GET /extensions`

List all loaded MCP extensions and their tools.

**Response 200**
```json
{
  "extensions": [
    {
      "name": "giap",
      "kind": "builtin",
      "description": "Built-in GIAP tools",
      "tools": ["giap__get_current_weather", "giap__recall_memories"],
      "enabled": true,
      "status": "connected",
      "last_error": null
    }
  ]
}
```

| Code | Meaning |
|------|---------|
| 503 | Extension manager not available |

---

#### `POST /extensions`

Register a new MCP extension. The config is persisted so the extension reconnects on restart.

**Request (stdio)**
```json
{
  "name": "my-extension",
  "kind": "stdio",
  "command": "python",
  "args": ["path/to/server.py"],
  "env": {"API_KEY": "secret"},
  "description": "My custom extension"
}
```

**Request (HTTP)**
```json
{
  "name": "remote-ext",
  "kind": "streamable_http",
  "uri": "http://192.168.1.50:3000/mcp",
  "description": "Remote MCP server"
}
```

**Response 201** -- extension info object (same shape as GET response items)

| Code | Meaning |
|------|---------|
| 400 | Invalid request body |
| 503 | Extension manager not available |

---

#### `DELETE /extensions/{name}`

Remove an extension and delete its persisted config.

**Response 204** -- no body

| Code | Meaning |
|------|---------|
| 404 | Extension not found |
| 503 | Extension manager not available |

---

#### `PATCH /extensions/{name}`

Enable or disable an extension without removing its config. Disabled extensions are excluded from future agent sessions.

**Request**
```json
{ "enabled": false }
```

**Response 200**
```json
{ "name": "my-extension", "enabled": false }
```

| Code | Meaning |
|------|---------|
| 404 | Extension not found |
| 503 | Extension manager not available |

---

#### `GET /agent/tools`

List all MCP tools currently available across all loaded extensions.

**Response 200**
```json
{
  "tools": [
    {
      "name": "giap__get_current_weather",
      "extension": "giap",
      "description": "Get current weather at the configured location."
    }
  ],
  "total": 9
}
```

---

#### `GET /dev/goose`

Reports Goose agent status, loaded extensions, and tool count. Useful for verifying extensions loaded correctly.

**Response 200**
```json
{
  "goose_active": true,
  "extension_count": 2,
  "extensions": [
    { "name": "giap", "kind": "builtin", "tools": ["giap__get_current_weather"] },
    { "name": "my-ext", "kind": "stdio", "tools": ["my-ext__greet"] }
  ],
  "tool_count": 10
}
```

---

### Marketplace Endpoints

The marketplace provides a curated registry of popular MCP extensions for one-click installation.

#### `GET /marketplace`

List all available extensions in the curated registry.

**Response 200**
```json
{
  "extensions": [
    {
      "id": "filesystem",
      "name": "Filesystem",
      "description": "Read, write, and search files on the local filesystem.",
      "kind": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/"],
      "category": "productivity",
      "author": "Anthropic",
      "tools": ["read_file", "write_file", "list_directory"],
      "featured": true
    }
  ]
}
```

| Code | Meaning |
|------|---------|
| 503 | Marketplace not configured |

---

#### `POST /marketplace/{id}/install`

Install a marketplace extension by its registry ID. Looks up the extension in the catalogue, registers it with the extension manager, and persists the config.

**Response 201** -- extension info object

| Code | Meaning |
|------|---------|
| 404 | Extension ID not found in marketplace |
| 503 | Marketplace or extension manager not available |
