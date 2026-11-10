# Goose Integration

Goose (the `goose/` submodule) is GIAP's live agent engine. `GooseAdapter`
(`crates/pond-adapters-goose/src/goose_agent.rs`) implements `pond-core`'s `Agent` port on it; every
chat turn, voice turn and scheduled run goes through it. This page describes how the two fit
together. Which patches the fork carries, and how to sync it, is in
[Goose Submodule Patch Management](../goose-patch-management.md).

## Agent backends

`pond-server serve --agent <backend>` (default `goose`). The `agent_backend` setting accepts only
`goose` (`AGENT_BACKENDS` in `crates/pond-core/src/user_data/domain/settings.rs`), so the other
backends are reachable only through the flag:

| Backend | What runs | Build |
|---|---|---|
| `goose` | `GooseAdapter` | default feature `goose-agent` |
| `mistralrs` | `pond-adapters-mistralrs`, a goose-free loop against a mistral.rs server ([direct agent](mistralrs-direct-agent.md)) | feature `mistralrs-agent`, off by default |
| `pond` | `pond-agent` + `pond-inference` | feature `pond-agent`, off by default. Quarantined: `PUT /api/v1/settings` answers 422, and `serve` rewrites a stored `pond` to `goose` at startup |
| `mock` | `MockAgent` | always |

`McpToolDispatcher` (`crates/pond-mcp-server/src/dispatcher.rs`), which calls the builtin MCP servers
directly without Goose, exists for the quarantined `pond` path only.

## Build wiring

- The goose crates are workspace members, built as path dependencies:
  `pond-adapters-goose` and `pond-adapters-local-inference` depend on `goose` with
  `default-features = false` and `features = ["rustls-tls", "local-inference"]`, plus
  `goose-providers`.
- Cargo resolves the goose crates' `workspace = true` entries against GIAP's root `Cargo.toml`, so
  that file mirrors `goose/Cargo.toml`'s `[workspace.dependencies]` and its `[patch.crates-io]`
  entries (the `pctx_*` crates from `goose/vendor/`). `rmcp` is pinned to `=2.1.0` in both.
- `llama-cpp-2` and `llama-cpp-sys-2` are pinned `=0.1.156` and come from the private
  `jarida-io/llama-cpp-rs-giap` fork, so any build needs a git credential that can read it.
- `pond-core` never imports goose. CI's fast crates compile without the submodule; the production
  check (`cargo check -p pond-server -p pond-adapters-goose --all-targets`) is what builds it.

## Where Goose keeps its state

At startup `pin_goose_state_under` (`crates/pond-server/src/main.rs`) sets `GOOSE_PATH_ROOT` to
`<data_dir>/engine`, so Goose's own session store and LiteRT-LM snapshots live in the data directory.
GIAP's chat history is in `pond_system.db`; the REST API never reads Goose's store.

Each GIAP session maps to one Goose session (Goose mints ids like `YYYYMMDD_N`).
`resolve_goose_session` checks an in-memory map, then the pairing persisted in GIAP's session storage
(re-validated: the Goose session must still exist and be named after the GIAP id), then creates and
hydrates a new one. Turn persistence stays in `ChatService`.

## Providers

`chat_provider` (with `chat_model`) chooses the Goose provider; a change rebuilds it on the next turn.

| `chat_provider` | Goose provider | Notes |
|---|---|---|
| `local` / `gguf` | `LocalInferenceProvider`, in process | A `.gguf` runs on llama.cpp, a `.litertlm` on the LiteRT-LM backend ([LiteRT-LM](litert-lm.md)). The model is registered in Goose's local model registry (`register_gguf_model_from`, `litert_model::register`); a vision encoder already on disk is attached, and on the budgeted device (the Orin) a speculative-decoding drafter (`mtp_drafter`) |
| `ollama` | Ollama | `GIAP_OLLAMA_URL`, default `http://127.0.0.1:11434` |
| `llamafile` | Ollama-compatible client at the llamafile URL | |
| `mistralrs` | OpenAI-compatible client | `GIAP_MISTRALRS_URL`, default `http://127.0.0.1:9002`; Mac-only experiment ([mistral.rs provider](mistralrs-provider.md)) |
| `mesh` | `MeshProvider` (`mesh_provider.rs`) | Inference on a peer pond over libp2p; needs the `mesh` feature and `mesh_enabled` |

An empty `chat_model` builds nothing; an unknown provider keeps the current one.

### The provider shim

Every provider is wrapped in `GiapProviderShim` (`crates/pond-adapters-goose/src/provider_shim.rs`),
which sees the final `(system, messages, tools)` of each call:

- rebuilds the system prompt as GIAP's prefix plus the per-turn appendix (extras, skills) whenever
  the incoming one is Goose's default ("general-purpose AI agent called goose") or GIAP's with
  something appended;
- removes any tool outside the session's allow-set (`SessionControls`), so a narrowed or guest turn
  cannot be offered a withheld tool; `GIAP_NO_TOOLS` removes all;
- minifies tool schemas and sorts tools core-first (`prefix_sort_key`) so turns share a KV prefix;
- handles pictures in tool results and history when the engine cannot read them;
- passes the call through the egress gate;
- writes the final payload to `$GIAP_CAPTURE_PAYLOAD` when that variable names a directory.

### Environment GIAP sets for Goose

`goose_env_knobs` exports these when the provider or effective context changes:

| Variable | Value | Why |
|---|---|---|
| `GOOSE_CONTEXT_LIMIT` | the effective context | Goose otherwise assumes 128K |
| `GOOSE_AUTO_COMPACT_THRESHOLD` | unset | Goose's default |
| `GOOSE_TOOL_PAIR_SUMMARIZATION` | unset | Goose's default |
| `GOOSE_MAX_EMPTY_TURN_RETRIES` | `0` | GIAP recovers an empty turn itself (`EMPTY_TURN_STEER`) with a changed prompt |
| `GOOSE_MAX_TOOL_RESPONSE_SIZE` | the context size, local only | One tool result cannot fill the window |
| `GOOSE_DISABLE_MOIM` | `1`, local only | Goose's `<turn-context>` block would move the prefix every turn |

## Tools

- **Builtins**: `register_giap_extensions` (`giap_registration.rs`) registers each enabled `giap-*`
  MCP server in Goose's builtin registry before the adapter is built; `GooseAdapter` then adds them
  to sessions in bulk. See [GIAP MCP Server Design Standard](mcp-server-standard.md).
- **Added extensions**: `GiapGooseExtensionManager` (`extension_manager.rs`) adds stdio and HTTP
  servers to the `giap-extensions` session. See [Building Extensions](extensions.md).
- **Selection**: with `tool_selection_mode` `relevant` or `minimal`, `GooseAdapter` picks tool groups
  per session (`pond_core::mcp::services::tool_selection`) and hands the shim the allow-set. In
  `relevant` every later message is scored too and its groups are added (`union_groups`), never
  removed; `giap-toolkit__enable_tool_group` widens it for the rest of the session. Neither goes
  beyond what the speaker is permitted.
- Goose runs in `GooseMode::Auto`: no per-call approval prompts.

## Other pieces in the adapter crate

| Module | Role |
|---|---|
| `provider_adapter.rs` | `GooseProviderAdapter`: a Goose provider as a `pond-core` `LlmProvider`, for side calls. `LocalInferenceLlmAdapter` (`pond-adapters-local-inference`) wraps it |
| `orchestrator.rs` | `GooseOrchestrator`: runs `delegate` tasks (`giap-orchestrator`) as Goose child agents with GIAP's own prompt |
| `compaction_prompts.rs`, `prompts/` | The compaction prompts used on the budgeted device |
| `vision_encoder.rs` | Verifies and attaches a vision encoder already on disk (the download pipeline fetches it) |
| `mtp_drafter.rs` | Attaches, and downloads if missing, the speculative-decoding drafter |
| `litert_model.rs` | Registry rows for `.litertlm` models and the device's LiteRT-LM environment |
| `shutdown.rs` | `release_local_models`: unloads models before exit, or LiteRT-LM's exit-time destructors crash |
| `goose_nudges.rs` (tests only) | Reads `goose/crates/goose/src/agents/agent.rs` and fails if the completeness-check wording reverts |
