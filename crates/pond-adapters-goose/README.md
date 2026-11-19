# pond-adapters-goose

Wraps [goose](https://github.com/aaif-goose/goose), the AAIF's open-source agent, pinned as the `goose/`
submodule, as the live agent engine. Everything GIAP knows about Goose's types stays in this crate.

| Type | Port it implements | Role |
|---|---|---|
| `GooseAdapter` (`goose_agent.rs`) | `AgentPort`, `ToolSelectionControl` | Runs each turn through Goose's agent loop and streams events back as `pond-core` types; maps GIAP session ids to Goose sessions |
| `GiapGooseExtensionManager` (`extension_manager.rs`) | `ExtensionManagerPort` | Adds, removes and reports MCP extensions |
| `GooseProviderAdapter` (`provider_adapter.rs`) | `LlmProvider` | Exposes a Goose `Provider` as a plain `LlmProvider`; `pond-adapters-local-inference` wraps it for in-process GGUF |
| `GooseOrchestrator` (`orchestrator.rs`) | `Orchestrator` | Runs authorised `TaskSpec`s as Goose child agents |

Supporting modules:

| Module | Why it exists |
|---|---|
| `provider_shim.rs` | Last-mile veto at `Provider::stream`: restores GIAP's system prompt and drops tools outside the allow-set |
| `giap_registration.rs` | Registers GIAP's MCP servers in Goose's builtin extension registry |
| `litert_model.rs` | Registers `.litertlm` models with Goose's `litert` backend |
| `mtp_drafter.rs` | Attaches a speculative-decoding drafter, downloading it if missing |
| `vision_encoder.rs`, `registry_rows.rs` | Stamps the vision encoder (mmproj) onto engine registry rows |
| `model_traits.rs`, `token_counter.rs` | Model traits read from the GGUF; token counting |
| `compaction_prompts.rs`, `src/prompts/` | The bounded compaction prompts used on the device |
| `mesh_provider.rs` | Exposes a mesh peer's `LlmProvider` as a Goose provider |
| `shutdown.rs` | Releases local engines on exit |

GIAP's changes to Goose itself live on the fork the submodule points at; see
[docs/goose-patch-management.md](../../docs/goose-patch-management.md).

## Tests

```bash
cargo test -p pond-adapters-goose                 # slow cold: compiles the submodule
cargo test -p pond-adapters-goose -- --ignored    # live tests, need a model on disk
```
