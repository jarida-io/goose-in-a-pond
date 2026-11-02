# Model management: types, ports and roles

The structure behind the Models page: the domain types, the ports and their adapters, the roles a
model can take, and how a role assignment reaches the engine that serves a turn. What the catalogue
holds and how files are acquired is in [models.md](models.md); which engine runs a turn and how it
is tuned is in [inference_optimization.md](inference_optimization.md). The general port/adapter
pattern is in [creating-ports-and-adapters.md](creating-ports-and-adapters.md).

## Overview

```
CompositeModelCatalogProvider (compiled list + a running Ollama)
        |  model_service::apply_catalog: upsert, then prune what went stale
        v
  models table  <-- ModelRepository -->  model_role_assignments
  (catalogue)                            (which model does which job)
                                                 |
                       ModelRole::settings_mirror | sync_assignments_to_settings (startup)
                                                 v
                         settings keys: chat_provider, chat_model, tool_model,
                         active_whisper_model, active_tts_model, voice_tts_voice,
                         active_embedding_model
                                                 |
                                                 v
        GooseAdapter builds the chat provider from chat_provider + chat_model;
        the voice pipeline and embedder read their own keys
```

The catalogue and the assignments live in `pond_system.db`. The settings keys are a mirror the
running code reads; an assignment is written first and the keys follow it.

## Domain types

All in `crates/pond-core/src/models/domain/`.

| Type / module | What it is |
|---|---|
| `ModelCategory` (`model_record.rs`) | `gguf`, `litert`, `llamafile`, `ollama` (language models); `whisper`; `tts_kokoro`, `tts_http`, `tts_piper` (legacy rows only, never synced to settings); `embedding` |
| `ModelRecord` (`model_record.rs`) | One catalogue row. `id = "{category}/{name}"`. Family-specific fields are `Option`s: LLM (`hf_id`, `ram_estimate_mb`, `recommended_role`, `context_length`, `quantization`), ASR (`asr_language`, `asr_size`), TTS (`tts_engine`, `tts_voice_name`, `config_*`, `tts_url`) |
| `ModelRoleAssignment` (`model_record.rs`) | `role` to `model_id` |
| `BinaryRecord` (`model_record.rs`) | A versioned per-platform tool binary. Part of the catalogue port's return type; no provider supplies one today |
| `Engine`, `EngineAvailability` (`engine.rs`) | The engine a conversation category runs on, and whether this process can run it |
| `ModelRole` (`model_role.rs`) | The seven roles, which categories each accepts, and the settings keys each owns |
| `CURATED` (`curated.rs`), `RECOMMENDED` (`recommended.rs`) | GIAP's pinned picks and its suggestions among them |
| `taxonomy.rs` | Provenance, kind (conversation or helper) and acquisition, derived from a row |
| `model_layout.rs` | The one mapping from (category, file) to a path |
| `vision_pairing.rs`, `vision_encoder.rs` | Which picture add-on a model needs, and its state on disk |
| `drafter.rs` | The pinned MTP drafter for a Gemma 4 E2B or E4B chat model |
| `device_budget.rs`, `device_profile.rs` | Memory budgets, context sizing, and the Jetson emulation switch |
| `litert.rs` | LiteRT-LM model files and engine options |
| `gguf.rs`, `model_probe.rs` | GGUF header parsing and what a model's chat template says it can do (tools, thinking) |

## Roles

| Role | Accepts | Settings it writes | Read by |
|---|---|---|---|
| `chat` | gguf, litert, llamafile, ollama | `chat_provider`, `chat_model` | `GooseAdapter` provider build |
| `think`, `task` | the same | nothing | nothing selects a model by these yet |
| `tool` | the same | `tool_model` | the tool-call argument repair helper |
| `asr` | whisper | `active_whisper_model` | the voice pipeline |
| `tts` | tts_kokoro, tts_http, tts_piper | `active_tts_model` (and `voice_tts_voice` for Kokoro) | Kokoro output |
| `embedding` | embedding | `active_embedding_model` | the embedder; the model's row picks the engine, and `embedding_provider = "none"` turns it off |

`ModelRole::loads_a_model` is false for think and task, so nothing done on behalf of their
assignments (the boot restore included) acts on them. `ModelRole::rebuilds_llm_provider` is true
only for chat: rebuilding for another role would throw away the KV prompt cache.

`chat_provider` is derived from the chat model's category by `ModelCategory::runtime_provider`:
`gguf` and `litert` map to `local`, `ollama` to `ollama`, anything else to `llamafile`. The other
accepted values (`mistralrs`, `mesh`) are set through `PUT /api/v1/settings` and are rewritten by
the next boot's `sync_assignments_to_settings` (see [mistralrs-provider.md](mistralrs-provider.md)).

## Ports and adapters

| Port (`crates/pond-core/src/models/ports/`) | Adapter | Notes |
|---|---|---|
| `ModelRepository` | `SqliteModelRepository` (`crates/pond-infra/src/sqlite_model_repository.rs`) | Catalogue rows and role assignments |
| `ModelCatalogProvider` | `CompositeModelCatalogProvider`, `StaticModelCatalogProvider`, `OllamaCatalogProvider` (`crates/pond-server/src/composite_model_catalog_provider.rs`) | `fetch()` takes no URL: the list is compiled in, plus whatever Ollama serves |
| `ModelDownloader` | `HttpModelDownloader` (`crates/pond-server/src/http_model_downloader.rs`) | Used by the CLI; the REST routes download through `model_acquisition.rs` and pond-hf-cache |
| `ModelStorage` | `FilesystemModelStorage` (`crates/pond-server/src/filesystem_model_storage.rs`) | Paths come from `model_layout.rs` |
| `ModelScheduler` | `ResourceAwareModelScheduler`, `NoopScheduler` (`crates/pond-adapters-local-inference/src/scheduler.rs`) | Memory status for `GET /models/memory-status` and wake-word notifications; not a download queue |
| `LlmProvider` (`provider.rs`) | `LocalInferenceLlmAdapter`, `OllamaProvider`, `LlamafileProvider` | The direct, non-agent path in `AppState.llm_provider`; turns go through the agent instead |
| `Agent` (`agent.rs`) | `GooseAdapter`; `MistralRsAgent` behind `mistralrs-agent`; `PondAgent` (quarantined) | The serving path |
| `InferenceProvider` (`inference.rs`) | `pond-inference`, `MistralRsProvider` | Feeds the quarantined `pond-agent` loop and the mistral.rs checkpoint, not the live path |
| `EmbeddingProvider` (`embedding.rs`) | fastembed (`pond-infra`), the GGUF embedder (`pond-inference/src/embedding.rs`), both behind `ActiveEmbedder` (`pond-core/src/models/services/active_embedder.rs`) | The Memory-role model, switched on a settings save without a restart; built by `EmbedderFactory` (`pond-server/src/embedder_factory.rs`) |

## Lifecycle

1. **Seed.** At startup (`ModelService::seed_catalog`), on `POST /api/v1/models/registry/refresh`
   and on `POST /api/v1/models/scan`, `model_service::apply_catalog` upserts what the catalogue
   provider returns, marks `downloaded` from the disk, and prunes stale rows (rules in
   [models.md](models.md#seeding-and-pruning)).
2. **Sync.** `sync_assignments_to_settings` (`crates/pond-server/src/main.rs`) writes each stored
   assignment into its settings keys at boot. A `tts_piper` assignment is skipped: Kokoro is the
   only TTS engine and chooses its own voice.
3. **Assign.** `POST /api/v1/models/{category}/{name}/activate` with `{"role": "chat"}` (or the CLI's
   `pond-server models activate <category> <name> --role chat`) checks the role accepts the
   category (400), refuses an engine this process cannot run (409), stores the assignment, writes
   the role's settings keys, and for a language-model role logs a spill warning when the model is
   over the memory budget and rebuilds the provider. A changed chat model also warms the prompt
   prefix in the background.
4. **Download.** Only on a person's request; see [models.md](models.md#acquisition).
5. **Serve.** `GooseAdapter` builds its provider from `chat_provider` and `chat_model` on the next
   turn (`crates/pond-adapters-goose/src/goose_agent.rs`); for `local` it registers the file in
   goose's engine registry, which picks llama.cpp or LiteRT-LM by the row's `backend_id`.

## Adding a conversation engine

The four engines are a closed set (`Engine::ALL`), and each step below has a test that fails when
one is missed:

1. **Category.** Add the `ModelCategory` variant with `as_str`/`from_str`, `is_llm`, and
   `runtime_provider`; add it to `LLM_CATEGORIES` in `model_role.rs`.
2. **Engine.** Add the `Engine` variant, its `category`, `id`, `label`, `file_format`,
   `in_process` and `backend_id` (`every_conversation_category_has_exactly_one_engine`).
3. **Storage.** Add the directory to `model_layout::dir_for`.
4. **Provider.** If goose already has a provider for it, add an arm to the provider build in
   `GooseAdapter` (the `match settings.chat_provider.as_str()` there) and wrap nothing else: every
   provider goes through `GiapProviderShim`. Add the provider id to `ON_DEVICE_PROVIDERS` or
   `HOSTED_PROVIDERS` in `crates/pond-core/src/models/services/context/model_class.rs` so the
   context governor classes it.
5. **Availability.** If the engine needs a library the pond does not build, extend
   `EngineAvailability` and decide it at startup, as `litert_runtime.rs` does for LiteRT-LM.
6. **UI.** The dashboard reads the engine from `GET /api/v1/models`; add its label and any
   unavailable note in `pond-desktop/src/sections/models/`.
