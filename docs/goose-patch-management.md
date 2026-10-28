# Goose Submodule Patch Management

GIAP builds Goose from source. `goose/` is a submodule of
[jarida-io/Goose](https://github.com/jarida-io/Goose), a fork of
[aaif-goose/goose](https://github.com/aaif-goose/goose). The fork's `main` is upstream (last
merged 2026-07-25, upstream tip `192b5db8b`) plus the GIAP patch set below. A patch missing from
this document is one that gets dropped at the next sync.

| | |
|---|---|
| Submodule | `.gitmodules`: `https://github.com/jarida-io/Goose`, `branch = main` |
| Pinned commit | `bc0993869`, the tip of fork branch `fix/jetson-model-load-memory`: fork `main` (`10497598a`) plus the model-load memory fix |
| How CI gets it | the recorded SHA: `git submodule update --init --recursive --depth=1` (rust job), `submodules: recursive` (live-test job) |
| Shared pins | `rmcp = "=2.1.0"` and `llama-cpp-2 = "=0.1.156"` in both workspaces |

```bash
git submodule status                              # the recorded SHA
git -C goose fetch origin
git -C goose merge-base --is-ancestor "$(git ls-tree HEAD goose | awk '{print $3}')" origin/main \
  && echo "pin is on fork main"
```

## Patch set

Commits are in the fork. "Up" says whether the patch is fit to send upstream as written.

### Agent loop (`crates/goose/src/agents/`)

| Patch | What and why | Files | Commits | Up |
|---|---|---|---|---|
| Ollama tool-less retry | When Ollama rejects tools with HTTP 400 ("does not support tools"), retry the turn without tools (`is_tools_unsupported_error`; `stream` retries, `stream_inner` is one attempt), so a model without tool support answers as a plain chat model. | `crates/goose-providers/src/ollama.rs` | re-ported in the 2026-07 sync | yes |
| A reasoning-only turn is empty | `provider_produced_content` returns false for `Thinking`/`RedactedThinking`, so a turn with only reasoning reaches the empty-turn path instead of ending in silence. The retry budget is overridable with `GOOSE_MAX_EMPTY_TURN_RETRIES` (default 3); at 0 the `EMPTY_TURN_MESSAGE` is yielded but not persisted. GIAP sets 0 and re-engages with a changed prompt (`EMPTY_TURN_STEER` in `pond-adapters-goose/src/goose_agent.rs`), because the built-in retry re-sends an unchanged conversation. | `agent.rs` | `29ef609cc` | yes |
| Session-scoped goal | `Agent::goal` is one slot on a shared agent, so one conversation's goal was injected into another's turn. Adds `session_goals` keyed by session id with `set_session_goal`/`get_session_goal`; the completeness check prefers it; cleared on turn exit. | `agent.rs`, `tests/agent.rs` | `9cf946903` | yes |
| The completeness check stays out of the answer | (a) The hidden nudges (completeness check, grind reminder, `/goal` kickoff) carry no "Goal:" label or emphasis, because models repeated that vocabulary to the household; `pond-adapters-goose/src/goose_nudges.rs` `include_str!`s `agent.rs` to guard the wording. (b) A check round with no tool calls is the check's own reply: it is not yielded or appended to `last_assistant_text`, and is kept in history as not user-visible. | `agent.rs` | `7849bd484`, `52a7827c2`, `87684c3c1` | yes |
| The check round stops at its first prose | The check's provider call runs inside `request_context::tool_calls_only`; the llama.cpp native-tools path stops at the first visible text before any tool call, and only after an answer was shown. LiteRT-LM and the emulated-tools path ignore the flag. | `agent.rs`, `goose-provider-types/src/request_context.rs`, `llamacpp/inference_native_tools.rs` | `e4052740d`, `80fc0d7d3` | yes |
| Model-config failures name the session and the keys | "Could not resolve model config" names the session id, the provider found or missing, and the keys consulted (`GOOSE_PROVIDER`, `GOOSE_MODEL`, goose config), in the same words at both resolution sites. | `agent.rs`, `platform_extensions/mod.rs` | `6a12584af` | yes |
| A panicking builtin costs only itself | `catch_unwind` around a builtin extension's spawn fn turns a startup panic into that extension's `ConfigError` instead of breaking every builtin loading beside it. | `extension_manager.rs` | `9a91cbf42` | yes |
| `<turn-context>` (MOIM) | `MIN_CONTEXT_FOR_MOIM` 32,000 -> 4,096 so the block reaches 16K-context models (not upstreamable as written; the upstream shape is an env override). The block is appended to the last user message instead of prepended, so prefix caches keep the prefix. `GOOSE_DISABLE_MOIM=1` removes the block and the system-prompt paragraph announcing it. GIAP sets `GOOSE_DISABLE_MOIM=1` for local providers (`goose_env_knobs`), so the threshold matters only for HTTP providers. | `moim.rs` | `b386ab1ff`, `36413f065`, `d4157795d` | partly |

### llama.cpp backend (`crates/goose-local-inference/src/llamacpp/`)

| Patch | What and why | Files | Commits | Up |
|---|---|---|---|---|
| `ProviderStats` parity | The backend fills TTFT, `model_load_ms`, `elapsed_ms` and `output_tokens`, plus new optional `prefill_ms` and `effective_context_tokens`. `prefill_ms` is stamped after the GPU synchronises, so Metal and CUDA do not bill prefill to the first token. | `goose-provider-types/src/conversation/token_usage.rs`, `lib.rs`, `llamacpp/*` | `961f1b0a5`, `afe7adc69` | yes |
| Prompt-session KV cache | One retained context per loaded model; the shared prompt prefix is reused across turns (partial KV removal to the divergence point), and small non-matching prompts run in a throwaway context. Model slots are keyed by canonical file path. Adds `ProviderStats.reused_prefix_tokens`. Multimodal turns reuse the prefix too. | `lib.rs`, `llamacpp/*`, `token_usage.rs` | `bfd2e854b`, `4c6cc973e` | yes |
| On-disk preamble snapshot | `prompt_snapshot.rs` saves prefix KV state to `<goose data>/prompt-cache/`: one snapshot per prompt shape, chosen by a ladder of prefix hashes, at most `MAX_SNAPSHOTS = 6` (LRU), written only on a cold turn and only between 1,024 and 6,144 tokens. Snapshots the prompts outgrew are deleted. Loaded tokens are checked to be a real prefix of the prompt. Logs to the `giap::kv` target. The snapshot key includes the KV types and `n_ubatch`. | `llamacpp/prompt_snapshot.rs`, `inference_engine.rs`, `mod.rs` | `d54376bb0`, `3c13ab814`, `789055d6b`, `a7bcc712b`, `759420e9b`, `12c5b7e73`, `7c02ca4c5`, `14ebdd383` | yes |
| KV cache type and physical batch | `ModelSettings` gains `type_k`/`type_v` (ggml type names such as `"q8_0"`) and `n_ubatch`, carried through the SDK DTO and both `management.rs` mappings. A quantised V cache requires flash attention, which is checked. `draft_n_max`/`draft_p_min` also survive a settings round-trip, and a pinned `context_size` is clamped to the model's trained window. | `local_model_registry.rs`, `inference_engine.rs`, `management.rs`, `goose-sdk-types/src/custom_requests.rs` | `943e92780`, `14ebdd383` | yes |
| Emulated tools keep the caller's system prompt | The emulated-tools path replaced `request.system` with a coding-agent preamble; it now keeps the caller's prompt. | `llamacpp/inference_emulated_tools.rs` | `daf700df9` | yes |
| Map the model, reserve one logits row | The model is mapped explicitly (`with_use_mmap(true)`), which llama.cpp's default turns off on an integrated GPU, and contexts reserve logits for one output (`with_n_outputs_max`, from `llama-cpp-rs-giap`). | `llamacpp/mod.rs`, `inference_engine.rs` | `8fb6988c2` | yes |
| Protect only a cache a chat filled | A request with no tools is one-shot work; its cache is marked `SessionKv::one_shot` and the next chat turn takes the slot. A full prefill into an existing context tries the preamble snapshot first. A chat's cache protects itself from another chat only while a generation ran on it within `CHAT_CACHE_PROTECTED_FOR` (60 s), so a new conversation takes a cache left idle instead of running beside it and prefilling again on its second turn; side calls are still sacrificed however long it has sat. | `llamacpp/mod.rs`, `inference_engine.rs` | `2d942ec47`, `10497598a` | yes |
| Windowed KV cache with checkpoints | Contexts keep a windowed sliding-window cache (`swa_full = false`); a partial resume goes through `SessionKv::window_resume`: in place, else from the newest matching window checkpoint (partial sequence state taken across a prefill's last 1,024 tokens, newest four kept), else refused. | `inference_engine.rs` | `8e2c61ca3` | yes |
| Speculative decoding (MTP), CUDA builds only | `llamacpp/mtp.rs` and `SessionCtx::Mtp`: a drafter loads beside the target only on a CUDA build, contexts with a drafter reserve logits for draft depth + 1, and picture turns run without it. The `cuda` feature maps to `llama-cpp-2/cuda-no-vmm` so two contexts can coexist. GIAP attaches a drafter already on disk on the budgeted device (`pond-adapters-goose/src/mtp_drafter.rs`). Built against `llama-cpp-2 =0.1.156`, which the parent patches to `jarida-io/llama-cpp-rs-giap`. Both generate paths still report `draft: None` in `ProviderStats`, so the in-process acceptance rate is not observed. | `llamacpp/{mtp.rs,mod.rs,inference_engine.rs,inference_native_tools.rs,inference_emulated_tools.rs}`, `Cargo.toml` | `9a630b207`, `a05b6a40b`, `6340d9892`, `313c2b739`, `d08c80d11`, `ca0d17f42`, `2a190e66b`, `e64640508`, `6589ba1cd` | no |
| Multimodal batch and encoder lifetime | A picture turn's prefill raises `n_ubatch` to at least 512 for the encoder's non-causal attention, and the vision encoder (`mtmd` context) loads per picture turn and is dropped after it instead of staying resident. | `llamacpp/mod.rs`, `inference_engine.rs` | `7244f01e9` | yes |
| Page cache and context fit on unified memory | On Linux with a GPU the backend releases a model's files from the page cache during the load (progress callback, every 256 MiB), after the target and drafter load, and after a picture encoder loads (`madvise(MADV_PAGEOUT)` on the file's read-only mappings, then `posix_fadvise(POSIX_FADV_DONTNEED)`), because nvmap does not reclaim page cache. A failed weight load or context creation is retried once after a release; a second failure with `MemFree` short is the public `load_error::ModelLoadError::OutOfGpuMemory`, carried in `ProviderError::ExecutionError` and read back with `from_provider_error`. On an integrated GPU the window is narrowed once per load to what `MemFree` buys (ladder 16,384 to 4,096). See [inference_optimization.md](developer/inference_optimization.md#unified-memory-and-the-models-own-page-cache). | `load_error.rs`, `llamacpp/{memory.rs,mod.rs,inference_engine.rs}`, `Cargo.toml` | `ef7a57bc3` | yes |
| Featured Gemma 4 models | `FEATURED_MODELS` lists the Gemma 4 sizes GIAP ships (E1B, E2B and E4B with their `mmproj`, and the larger ones), which gives them `ToolCallingMode::ForceNative` defaults and vision wiring. | `local_model_registry.rs` | re-ported in the 2026-07 sync | no |

### LiteRT-LM backend (`crates/goose-local-inference/src/litert/`)

| Patch | What and why | Files | Commits | Up |
|---|---|---|---|---|
| `litert` backend | A third `LocalInferenceBackend` that runs `.litertlm` models through the LiteRT-LM C API, chosen per registry entry by `backend_id: "litert"`, so the host keeps `chat_provider = "local"`. The library is loaded at run time with `libloading` from `GOOSE_LITERT_LIB_DIR`, `<exe dir>/litert-lm`, `<exe dir>` or `<goose data>/litert-lm/lib`; nothing native is built by cargo. One retained conversation per model; side calls run in a throwaway conversation. With GIAP's LiteRT-LM fork (`giap-main`) a diverged history is rematched, and the retained conversation is set aside as a KV snapshot in `<goose data>/litert-lm/kv-snapshots` (two kept; tool-less one-shot work is never saved) and restored on the next request of its family. Tool calls the parser rejects are kept and repaired by `fc_repair.rs`. `ModelSettings.litert` holds backend and speculative-decoding settings. GIAP builds and records the library with `giap.sh litert`. | `litert/{mod.rs,ffi.rs,convert.rs,fc_repair.rs}`, `lib.rs`, `local_model_registry.rs`, `management.rs`, `Cargo.toml` | `f253d1330`, `e27ad7334`, `bf5d9d524`, `6c38f5b32`, `424a692a5` | yes |

### Context management and the provider request context

| Patch | What and why | Files | Commits | Up |
|---|---|---|---|---|
| Compaction keeps its order across a reload | The copy of the user's last message kept after a compaction summary is stamped when pushed, never earlier than the message ahead of it. Before, a summary that took over a second sorted after the copy on reload and the next turns read a rewritten history. | `crates/goose/src/context_mgmt/mod.rs` | `f06a009b8` | yes |
| `request_context` task-locals | A new `goose-provider-types` module that tells providers about a call: `replacing_history` (a compaction's summary call, so LiteRT-LM skips a snapshot its family already has), `for_session` (the session a call is for, so the skip applies only when the summary replaces that session's history), and `tool_calls_only` (above). | `goose-provider-types/src/request_context.rs`, `crates/goose/src/{context_mgmt/mod.rs,session_context.rs}`, `goose-local-inference/src/{backend.rs,lib.rs,litert/mod.rs}` | `e45f6b357`, `b903b2e58`, `e4052740d` | yes |

### Security and dependency updates

| Patch | What and why | Files | Commits |
|---|---|---|---|
| RMCP 2.1 | Pins `rmcp = "=2.1.0"` for the advisories listed in the fork's `SECURITY-MCP.md`. Custom HTTP clients (OAuth, PCTX) disable redirects, since supplying a client bypasses RMCP's redirect policy. Four PCTX crates are vendored with compatibility patches (`vendor/SECURITY-PATCHES.md`) so optional code-mode survives; the parent mirrors them in its `[patch.crates-io]`. | `Cargo.toml`, `extension_manager.rs`, `platform_extensions/code_execution.rs`, `vendor/pctx_*` | `e6189a2b7`, `ad36c7fbb` |
| Lockfile bumps | Document and optional Nostr dependencies, the QUIC library, rustls 0.23.46. | `Cargo.lock`, `crates/goose/Cargo.toml`, `crates/goose-mcp/Cargo.toml` | `52d8eb9ef`, `d395e8ff1`, `140ba3b23` |

### Open items

| Item | Where | Disposition |
|---|---|---|
| `giap_system.md` | `crates/goose/src/prompts/` | Delete. Not in `TEMPLATE_REGISTRY`, so nothing renders it; a host overrides templates through `Paths::config_dir()/prompts/<name>` without a patch. |
| Four `.snap.new` insta files | `crates/goose/src/agents/snapshots/` | Delete. They are insta's failure output and stale. |
| `McpClient` session id overwrite | `crates/goose/src/agents/mcp_client.rs` (`set_session_id`) | Decide. Upstream's cross-session `assert!` is a `tracing::debug!` plus overwrite on the fork; undocumented until now. |

## Proposed patch: `Auto` mode must still honour `NeverAllow`

**Not applied.** GIAP narrows the tool surface per session (a Guest turn does not get the memory
tools), but the narrowing is schema-only: `provider_shim.rs::enforce_tools` filters the tools the
provider sees, while Goose keeps every extension loaded and dispatches any `ToolRequest` the
model makes. `goose_agent.rs` drops the call event and result of a withheld tool
(`suppressed_tool_ids`), which closes the disclosure, but the tool has still run.

`ToolInspectionManager` is unreachable from a host (`pub(super)`, built in a private function),
and writing `PermissionLevel::NeverAllow` through `PermissionManager` does nothing because
`permission_inspector.rs` returns `Allow` for `GooseMode::Auto` before it reads the stored
permission. GIAP runs in `Auto`; leaving it would turn on approval prompts for every call.

The patch, in `crates/goose/src/permission/permission_inspector.rs`:

```rust
GooseMode::Auto => match permission_manager.get_user_permission(tool_name) {
    Some(PermissionLevel::NeverAllow) => InspectionAction::Deny,
    _ => InspectionAction::Allow,
},
```

The existing `auto_allows` test still passes (no stored permission); add a case that `Auto` plus
`NeverAllow` denies. On the GIAP side, write `NeverAllow` for the tools outside the session's
allow-set before `Agent::reply`. `PermissionManager` is process-global and keyed by tool name,
so concurrent sessions of different scope need the write serialised with the turn, or a
session-scoped variant upstream.

## Syncing with upstream

The fork's history contains merges, so a sync is a merge, not a rebase.

```bash
cd goose
git remote add upstream https://github.com/aaif-goose/goose   # once
git fetch upstream main
git checkout -B sync-staging origin/main
git merge upstream/main        # resolve, re-porting the patch set where upstream moved code
cd ..
cargo check --locked -p pond-server -p pond-adapters-goose --all-targets
```

Then, before the parent commit:

1. **Mirror the dependencies.** Cargo resolves the goose crates' `workspace = true` entries
   against the parent `Cargo.toml` and ignores goose's own `[patch]`. Copy new or changed keys
   from `goose/Cargo.toml [workspace.dependencies]` into the parent (union the features GIAP
   needs). Where goose moved to a major a pond crate is not ready for, keep goose's version in
   the workspace key and pin the old one in the pond crate (as for `rand`, `sha2`, `thiserror`).
   Replicate any `[patch.crates-io]` entry goose's graph needs (today the four `pctx_*` crates).
2. **Land the fork first.** Open a pull request from `sync-staging` into fork `main` and merge
   it. The parent must only ever pin a commit reachable from fork `main`.
3. **One parent commit** moves the submodule pointer together with the parent-side port and this
   document. Do not move fork `main` underneath a parent that still targets the old API (for
   example RMCP 1.x); merge the fork and parent changes together.

After it lands, everyone runs `git submodule update --init --recursive`.

## Adding a GIAP patch

1. Branch in the fork from `origin/main`, commit, and open a pull request into fork `main`.
2. Merge it, then move the parent pointer: `git -C goose checkout <merged sha>` and commit `goose`
   with the parent change that needs it.
3. Add a row to the table above.

## Dropping a patch

When upstream lands an equivalent change, the next sync's merge resolves the patch away. Remove
its row here, and drop any parent-side guard that only existed for it.
