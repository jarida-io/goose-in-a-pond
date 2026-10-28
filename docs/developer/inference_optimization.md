# Inference: backends, selection and tuning

Which engines can serve a turn, how one is chosen, and the levers that decide how fast it is. The
model catalogue and downloads are in [models.md](models.md); the LiteRT-LM library build is in
[litert-lm.md](litert-lm.md); device procedure is in [jetson-dev-routine.md](jetson-dev-routine.md).

## The serving path

```
HTTP / voice turn
  -> ChatService (persistence)          crates/pond-core/src/shared/services/chat.rs
  -> Agent port: GooseAdapter            crates/pond-adapters-goose/src/goose_agent.rs
       goose::Agent::reply (tools over MCP, compaction, re-engagement)
  -> GiapProviderShim                    crates/pond-adapters-goose/src/provider_shim.rs
       re-imposes GIAP's system prompt and tool list, gates egress
  -> goose provider, by chat_provider
       local  -> LocalInferenceProvider  goose/crates/goose-local-inference
                   llama.cpp backend (.gguf) | LiteRT-LM backend (.litertlm)
       ollama | llamafile | mistralrs | mesh
```

## Choosing the agent backend

`serve --agent <name>`; the stored `agent_backend` is used when the flag is left at `goose`.

| Backend | How it is reached | Status |
|---|---|---|
| `goose` | Default. The only value `PUT /api/v1/settings` accepts (`AGENT_BACKENDS`) | The live path |
| `mistralrs` | `serve --agent mistralrs` on a build with `--features mistralrs-agent` | Mac-only checkpoint that bypasses goose; see [mistralrs-direct-agent.md](mistralrs-direct-agent.md) |
| `pond` | Rejected with 422 by the settings API and rewritten to `goose` at startup; the `pond-agent` feature is off by default | Quarantined. `pond-agent` and `pond-inference`'s `InferenceProvider` feed only this loop |
| `mock` | `serve --agent mock` | Tests and UI work without a model |

## Choosing the chat provider and engine

`chat_provider` and `chat_model` select the provider. Assigning a model to the chat role writes
both (`ModelCategory::runtime_provider`: `gguf` and `litert` become `local`, `ollama` stays,
anything else becomes `llamafile`). The provider is rebuilt on the next turn after a change.

| `chat_provider` | Engine | Where it runs | Notes |
|---|---|---|---|
| `local` (alias `gguf`) | goose's `LocalInferenceProvider` | In process | The model's registry row decides the backend: llama.cpp for a `.gguf`, LiteRT-LM for a `.litertlm` (`backend_id = "litert"`). `GooseAdapter` registers the file before the build (`register_gguf_model`, `litert_model::register`) |
| `ollama` | goose's Ollama provider | Ollama at `GIAP_OLLAMA_URL` (default `http://127.0.0.1:11434`) | Ollama runs on its own; the pond neither starts nor stops it. A running Ollama's models are listed in the catalogue |
| `llamafile` | goose's Ollama provider pointed at the llamafile | A child process `llamafile_process.rs` starts, on `GIAP_LLAMAFILE_PORT` (default 8080) | Set the port when pond-server itself holds 8080, as on the Jetson |
| `mistralrs` | goose's OpenAI provider | A mistral.rs server at `GIAP_MISTRALRS_URL` (default `http://127.0.0.1:9002`) | Mac evaluation only; see [mistralrs-provider.md](mistralrs-provider.md) |
| `mesh` | `MeshProvider` | A peer pond, over libp2p | Needs the `mesh` feature and `mesh_enabled`; no MCP tools over mesh |

An empty `chat_model` builds nothing: chat answers `409 {"code": "no_model"}` and nothing is
picked in its place. A second, non-agent provider is built from the same setting into
`AppState.llm_provider` (`build_provider` in `crates/pond-server/src/main.rs`) for routes that
call a model directly.

The LiteRT-LM backend needs its C library on the host. When it is missing, every `.litertlm`
model is listed as unavailable and refused with 409 rather than failing its first turn; see
[models.md](models.md#an-engine-this-pond-cannot-run).

## Builds and acceleration

| Host | Build | Acceleration |
|---|---|---|
| macOS | default features | Metal, automatic through llama-cpp-2. LiteRT-LM runs on WebGPU from a source checkout and on Metal in the app ([litert-lm.md](litert-lm.md#metal-or-webgpu-on-a-mac)) |
| Jetson Orin | `--features pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda` with `CMAKE_CUDA_ARCHITECTURES=87 CUDA_COMPUTE_CAP=87` | CUDA sm_87. The goose crate maps `cuda` to llama-cpp-2's `cuda-no-vmm`, the legacy pool, so a drafter's context and the target's can coexist |
| Jetson, plain `cargo build` | no `cuda` | CPU only, many times slower; the log line below gives it away |

`llama-cpp-2` and `llama-cpp-sys-2` are patched in the root `Cargo.toml` to the
`jarida-io/llama-cpp-rs-giap` fork, pinned by full SHA: 0.1.156 (which has MTP) with the
0.1.146 OpenAI-compatible template surface re-applied, plus allocation-failure fixes listed in the
fork's `llama-cpp-sys-2/POND-PATCH.md`. No published version has both APIs; the history is in
[jetson-mtp-in-process-scope.md](jetson-mtp-in-process-scope.md).

## Per-model settings

Before llama.cpp or LiteRT-LM loads a model, `LocalInferenceLlmAdapter::apply_model_settings`
(`crates/pond-adapters-local-inference/src/lib.rs`) stamps goose's registry row (and every other
row naming the same file). Hand edits to the registry are overwritten on the next provider build;
the code is the knob.

| Stamp | When | What it sets | Log line |
|---|---|---|---|
| `apply_jetson_settings` | CUDA builds, or any build under a `POND_DEVICE_PROFILE` | `n_gpu_layers 99`, the derived `context_size`, `n_batch 512`, `n_ubatch 256` (512 with a picture add-on), `n_threads 4`, flash attention, f16 K and V, no mlock, tool mode and thinking read from the model's chat template | `Applied Jetson Orin Nano CUDA settings`, after `Jetson context sized to fit this model's KV cache in the LLM budget` |
| `apply_platform_settings` | Every other build | `n_gpu_layers 99`, context sized by goose from free memory, `n_batch 512`, flash attention, template-derived tool mode and thinking | `Metal/platform settings applied to model ...`; on aarch64 Linux the same line begins `CPU-ONLY (built without the cuda feature)` |
| `apply_litert_settings` | LiteRT-LM rows | The engine window and execution backend (`pond_core::models::domain::litert::engine_options`) | `Applied LiteRT-LM settings` |

The drafter and picture add-on fields are kept per row: other code attaches them and the stamps
never overwrite them.

## The context window

- **Budgeted device** (the Orin, a CUDA build, or an emulation): `device_budget::device_window`
  sizes the window so the model's KV cache fits `LLM_BUDGET_MB` (7,620 MB total less 1,500 system,
  200 speech-to-text and 100 text-to-speech = 5,820 MB) after the weights and a 600 MB compute
  buffer. The KV cost per token comes from the GGUF header (`kv_cost_from_header`) for dense models
  and for sliding-window architectures whose ratio was confirmed on the device (`gemma4`);
  otherwise the 56 KiB/token fallback applies. The window floors to a multiple of 1,024 and stays
  between 2,048 and 16,384 (`MAX_CTX` is a latency cap: a cold 16K prefill is about 20 s on the
  Orin). A picture add-on is charged against the budget when declared; an allowlisted Gemma model
  keeps 16,384.
- **Unified memory, at load** (Linux with an integrated GPU: the Orin). The engine narrows the
  window it was asked for to what the memory free after the load buys (`MemoryFit` in
  `goose-local-inference/src/llamacpp/memory.rs`, logged as `kind="context_sized"`). Free is the
  kernel's `MemFree` once the model's files are out of the page cache, less the picture add-on's
  weights; from it come a 256 MiB compute reserve (not yet measured with one logits row; replace it
  with the `compute buffer size` llama.cpp logs on the Orin) and a 512 MiB margin. The KV cost per
  token is read from the model's metadata and charges every layer that owns KV the full window at
  the full key width, which over-states sliding-window layers under the windowed cache; the header
  does not say which layers slide. A window that fits is kept exactly. One that does not steps down
  16,384 / 12,288 / 8,192 / 6,144 / 4,096 and never below 4,096. The fit is fixed per load, so the
  retained context and the preamble snapshot's key do not move with free memory turn to turn. A
  prompt longer than a narrowed window is `ContextLengthExceeded`, which goose answers by compacting.
- **Any other machine**: goose sizes the context from free memory, capped by the model's own
  training length.
- **LiteRT-LM**: fixed per engine, 8,192 tokens on the budgeted device and 16,384 elsewhere
  (`DEVICE_CONTEXT_TOKENS`, `PLATFORM_CONTEXT_TOKENS`).
- **`context_window_override`** caps the window GIAP budgets history against; 0 (the default)
  uses the model's own. Leave it at 0 on the Orin: a larger value only makes GIAP budget history
  the engine cannot hold.

## The llama.cpp engine

In `goose/crates/goose-local-inference/src/llamacpp/`.

- **KV prompt-session cache.** The engine keeps the last conversation's KV and token ledger and
  plans each prompt against it (`PrefillPlan` in `inference_engine.rs`, logged as `prompt prefill
  plan`): `CreateContext` for the first turn after a load, `ReusePrefix` when the prompt extends
  what is cached, `SacrificialContext` for a prompt under half the size of a chat's cache that must
  not evict it, `FullPrefillInPlace` when the prefix changed. A side call with no tools (memory
  extraction, a title) is always sacrificed; another chat only while the cached one ran within the
  last 60 s (`CHAT_CACHE_PROTECTED_FOR`), so a new conversation takes a cache left idle, prefills
  in the full window and restores a preamble snapshot when one matches its shape. A
  `FullPrefillInPlace` on turn 2 of a session means something rewrote the prompt's start between
  turns.
- **Windowed sliding-window cache.** Contexts use `swa_full = false`: sliding-window layers keep only
  their window, and every partial resume goes through `SessionKv::window_resume`, which resumes in
  place, restores a window checkpoint, or refuses. On the Orin this took E4B's KV at 16K from 476 MiB
  to 152 MiB (q8_0) and decoded 8% faster at depth.
- **Stable-prefix snapshot on disk** (`prompt_snapshot.rs`). The longest token prefix every prompt
  for a model has shared (system prompt and tool schemas, never conversation) is saved under goose's
  data directory (`<data_dir>/engine/data/prompt-cache/`), keyed by model file, context size and
  layout, and read back on a cold start instead of re-decoded.
- **Prefix prewarm.** At startup and when the chat model changes, `spawn_prefix_prewarm`
  (`crates/pond-api/src/lib.rs`) runs one throwaway turn through the real chat path so the first
  real turn finds its prefix cached. Its progress is `state.warmup`.
- **Logits reservation.** Contexts reserve logits for one output (`with_n_outputs_max`), or for the
  draft depth plus one with a drafter attached, instead of a full vocabulary row per ubatch token.
- **Weights are mmapped**, so on the Orin the CPU-side tensors stay reclaimable page cache. The copy
  of the GPU-side weights the load leaves in the page cache is released (see [Memory fit](#memory-fit)).
  A picture add-on's encoder is loaded per picture turn, not held resident.

### Speculative decoding (MTP)

`goose-local-inference/src/llamacpp/mtp.rs` decodes with a Gemma 4 `gemma4-assistant` drafter
when the registry row names one, on **CUDA builds only**: verifying a few drafts costs about one
decode on the Orin (2.1-2.3x decode in process), while Metal measured 0.64-0.94x on live turns.
Output is token-identical to decoding without speculation; a test asserts it at temperature 0.

On a budgeted device every provider build calls `mtp_drafter::attach_drafter`
(`crates/pond-adapters-goose/src/mtp_drafter.rs`): it registers the pinned drafter for the chat
model's family and size (`pond_core::models::domain::drafter::drafter_for`) and points the rows
at it, or, when the file is missing, starts a background download through pond-hf-cache and
attaches it on a later build. There is no setting; a drafter that fails to load leaves the model
running without speculation.

LiteRT-LM's own multi-token prediction is a separate rule (`litert::engine_options`): off on the
budgeted device, on elsewhere except E2B on the CPU; `GIAP_LITERT_SPECULATIVE=on|off` overrides.

## Prompt-side levers

Most of a cold turn's time on-device is prefill of the prompt, and most of the prompt is tool
schemas ([prefill-cost-audit.md](prefill-cost-audit.md)).

| Setting | Values (default first) | Effect |
|---|---|---|
| `tool_selection_mode` | `all`, `relevant`, `minimal` | `relevant` sends core groups plus those scored relevant, fixed per session so the KV prefix holds; `minimal` sends only the toolkit hatch and loads groups through `enable_tool_group` |
| `thinking_mode` | `auto`, `on`, `off` | `auto` thinks only on models whose template supports it; on the Orin every thinking token is decode-bound silence |
| `reasoning_effort` | `brief`, `balanced`, `thorough` | Thinking length when thinking is on |
| `hybrid_compaction_enabled` | on | Compacts long conversations while the household is quiet, so the next turn does not stall for it |
| `goal_check_enabled` | on | A completeness check after the answer: one more inference per turn |

`goose_env_knobs` in `goose_agent.rs` sets goose's own knobs per provider, among them
`GOOSE_CONTEXT_LIMIT`, `GOOSE_AUTO_COMPACT_THRESHOLD` and, for local providers,
`GOOSE_DISABLE_MOIM=1`, which keeps goose's per-turn context block from moving bytes the engine
already cached.

Background model work (quiet compaction, memory consolidation and extraction, titling, proactive
review, summary refresh, index maintenance: `LaneJob` in
`crates/pond-core/src/user_data/services/inference_lane.rs`) runs one job at a time through the
inference lane (`crates/pond-server/src/inference_lane_runner.rs`), so background jobs never queue
on each other for the one model slot.

## Memory fit

On-device decode is memory-bandwidth-bound: on the Orin, roughly `tok/s = 102 / model_GB` when the
model is fully on the GPU. A model over the budget does not fail; it spills to the CPU and decodes
at a fraction of that.

- The Models page marks such a model "Too large" (`pond-desktop/src/api/modelFit.ts`, headroom
  1,024 MB) against `GET /api/v1/models/memory-status`.
- Assigning one to a language-model role logs a warning (`warn_if_model_spills`,
  `crates/pond-api/src/routes.rs`). Nothing refuses the load: the Jetson stamp still asks for
  `n_gpu_layers 99`.
- On the Orin, NvMap cannot reclaim page cache, so a load can fail with `NvMap ... error 12` while
  `free` shows gigabytes available. Drop caches with the pond stopped (see
  [jetson-power-loss-recovery.md](jetson-power-loss-recovery.md)) when the cache is other files'.

### Unified memory and the model's own page cache

The Orin's GPU allocates from system memory through nvmap, which fails a `cudaMalloc` when `MemFree`
runs low and does not reclaim page cache for it. llama.cpp maps the GGUF and copies the GPU-resident
weights out of the mapping, so a load leaves those bytes in the page cache as a second copy, and
llama.cpp reports `MemAvailable` (which counts that cache) as the device's free memory. On
2026-10-27, switching to Gemma 4 E4B (`UD-Q2_K_XL`, a 3.2 GB file, about 2.2 GB of it on the GPU)
failed every turn: with 4.0 GB of cache the 2,197 MiB weight allocation failed; after
`drop_caches` the weights loaded but the cache grew by 2.2 GB, 251 MB stayed free, and a 42 MiB
allocation and then the 256 MiB KV cache failed. The chat showed "Ran into this error: Execution
error: null result from llama cpp."

What the engine does now (`goose-local-inference/src/llamacpp/memory.rs`), on Linux with a GPU:

- **Releases the model's files from the page cache** while llama.cpp loads them (its progress
  callback, every 256 MiB), once the target and its drafter are loaded, and once a picture
  add-on's encoder is. A released file's pages this process maps are paged out with
  `madvise(MADV_PAGEOUT)` (Linux 5.4+; a no-op for file pages of a file the pond user neither owns
  nor can write), then the file is advised away with `posix_fadvise(POSIX_FADV_DONTNEED)`, which by
  itself skips mapped pages. Neither changes what the mapping reads: a CPU-side tensor row is read
  back from disk the next time a token needs it. Logged as `kind="page_cache_released"` with
  `MemFree` and `Cached` before and after.
- **Maps the model on the first attempt, reads it on a retry.** Without mmap llama.cpp copies the
  CPU-side tensors (1,872 MiB for E4B) into host memory nothing can reclaim; mapped, a token reads
  only its own rows. But a load maps with `MAP_POPULATE` and `MADV_WILLNEED`, so the whole file
  enters the page cache before the GPU buffers are allocated. On 2026-10-27 a released cache
  (1,874 to 4,915 MiB free) refilled to 3.3 GB within a mapped retry and the 2,197 MiB weight
  allocation failed again. So with unified memory the retry reads the file: the buffers are
  allocated first and the progress release keeps the cache small (`retry_mapped=false`).
- **Retries a failed allocation once** after releasing the loaded models' files, for the weights
  and for a context (`kind="model_load_retry"`, with free memory before and after). A second
  failure with `MemFree` short of what the stage needs plus the 512 MiB margin is
  `ModelLoadError::OutOfGpuMemory` (`goose-local-inference/src/load_error.rs`,
  `kind="model_out_of_gpu_memory"`); any other failure keeps llama.cpp's error.
- **Says so in the chat.** The provider shim counts that typed error per session and
  `GooseAdapter` replaces goose's "Ran into this error" prose with "The model could not load: the
  Pond is out of GPU memory. Try a smaller model on the Models page, or restart the Pond."
  (`kind="turn_out_of_gpu_memory"`), on both chat streaming endpoints.

The preamble snapshot slot (`kv snapshot slot created`) allocates no context; the snapshot is read
into the one context the turn already has. The drafter's context shares the target's KV.

A row the goose adapter registers for a model takes the device stamp of another row naming the
same file (`stamped_settings_for` in `goose_agent.rs`), so a live switch no longer leaves the row
it loads with goose's defaults.

## Measuring

- Every turn writes a row to `turn_metrics` in `pond_logs.db` (TTFT, prefill, prompt and completion
  tokens, decode rate, reasoning tokens, inference count).
- Per inference, with `RUST_LOG=info,goose_local_inference=debug,giap::kv=debug,pond_adapters_goose=debug`:
  `prompt prefill plan`, `kind="inference"`, `provider payload size`.
  `GIAP_CAPTURE_PAYLOAD=<dir>` makes the shim write each inference's final request.
- `scripts/pai-bench.sh` drives the eight PAI capabilities against a real model in a scratch pond.
- Run a release binary for any number: a debug build measured 1.5-2x slower on prefill and decode
  ([realtime-inference-audit-2026-09-24.md](realtime-inference-audit-2026-09-24.md)).
- `scripts/jetson-emu.sh` reproduces the Orin's decisions on a Mac, never its throughput, KV cost
  or free memory. Those come from the board.

Dated measurements: [orin-prefill-measurement.md](orin-prefill-measurement.md),
[prefill-cost-audit.md](prefill-cost-audit.md),
[realtime-inference-audit-2026-09-24.md](realtime-inference-audit-2026-09-24.md),
[jetson-engine-bakeoff.md](jetson-engine-bakeoff.md).
