# Models: engines, sources, picks and add-ons

How the pond describes and acquires models. The rule that shapes it: **a conversation model or a
picture add-on is downloaded or switched on only when a person chooses it.** What the pond fetches
on its own is listed under [Companion models](#companion-models): the boot restore of a model the
household already assigned, the speculative-decoding drafter on the Orin, and the speech, voice and
embedding files the voice pipeline and memory need. Each of those shows on the Models page or in
the log like any other download.

The type and port structure behind all of this is in [model_architecture.md](model_architecture.md);
which engine serves a turn, and how, is in [inference_optimization.md](inference_optimization.md).

## Engines

Every conversation row names the engine that runs it, derived from its category
(`crates/pond-core/src/models/domain/engine.rs`; nothing is stored):

| Engine | Category | File | Runs |
|---|---|---|---|
| llama.cpp | `gguf` | `.gguf` | in the pond's process (goose's `local` provider); can read pictures with an add-on |
| LiteRT-LM | `litert` | `.litertlm` | in the pond's process (goose's `local` provider, `litert` backend), on the GPU; text only |
| Ollama | `ollama` | none of the pond's | in the Ollama server |
| llamafile | `llamafile` | `.llamafile` | in its own child process |

`Engine::backend_id` is the goose registry's id (`litert` for LiteRT-LM, none for llama.cpp);
goose keeps its own copy private. Both in-process engines sit behind one `chat_provider`, `local`
(`gguf` is accepted as an alias): the chat model's file decides the backend.

### An engine this pond cannot run

LiteRT-LM is the one engine that needs a library the pond does not build itself (see
[litert-lm.md](litert-lm.md)). At startup `litert_runtime::ensure_library_dir` in
`crates/pond-server/src/litert_runtime.rs` checks every place goose's `litert` backend searches and
returns whether one holds the library; `serve` puts the answer in
`AppState::engine_availability` (`EngineAvailability` in `engine.rs`), logging a `WARN` with the
searched paths when there is none and LiteRT-LM models are on disk. Without it:

- `GET /api/v1/models` lists each `litert` row with `"available": false` and
  `"unavailable_reason": "litert_library_missing"`. Every other row carries `"available": true`; a
  client that predates the field reads its absence as available.
- `POST /models/{category}/{name}/activate` and `POST /models/{category}/{name}/download` answer
  `409 {"error": ..., "reason": "litert_library_missing"}` and change nothing.
- The dashboard keeps such a model listed but disabled, with "Needs the LiteRT-LM library: run
  giap.sh litert build" (`unavailableNote` in `pond-desktop/src/sections/models/modelsView.ts`, the string in
  `pond-desktop/src/i18n.ts`) in the chat's model switcher, the Models rows and pick cards; the settings chat
  picker leaves it out.

The decision is made once, so installing the library takes a pond restart. `PUT /api/v1/settings`
with a LiteRT-LM `chat_model` is not refused: a whole-settings save must not fail because of a
model chosen before the library went missing.

## Sources, purpose, acquisition

`crates/pond-core/src/models/domain/taxonomy.rs`, all derived from the row:

- **Provenance**: `catalogue` (one of GIAP's picks, or a bundled speech, voice or embedding row),
  `ollama` (listed by the Ollama server), `added` (has a download URL), `on_disk` (found by the
  scan). A pick that leaves the list but is still downloaded reads `added`.
- **Kind**: `conversation` or `helper`. FunctionGemma, `tool`- and `draft`-role rows, encoders,
  drafters (`mtp-*`, `dflash-*`, Gemma 4 `-assistant`) and speech or embedding files are helpers
  and are never offered as conversation. The disk scan skips companion files and helper GGUF
  architectures (`clip`, `dflash`, `*-assistant`, ASR, embeddings) altogether. An Ollama tag
  names no file, so only its words (asr, whisper, embed, functiongemma) make it a helper.
- **Acquire**: `download`, `external` (Ollama, HTTP voices, self-fetching embeddings) or
  `unavailable` (a file with no source).
- **Title**: a pick's own title; else, for a llama.cpp file the pairing table lists, the table's
  name for it ("SmolVLM 256M"); else a found file's header name; else the file's stem.

## The catalogue

`CompositeModelCatalogProvider` (`crates/pond-server/src/composite_model_catalog_provider.rs`)
assembles the catalogue from two sources, and one failing is skipped:

- `StaticModelCatalogProvider`: the Whisper models (`ggml-*.bin` from `ggerganov/whisper.cpp`),
  the Kokoro voices, the curated conversation picks, and the embedding rows (fastembed's
  `all-MiniLM-L6-v2` and `bge-small-en-v1.5`, which fetch themselves).
- `OllamaCatalogProvider`: whatever a running Ollama server lists.

Nothing is fetched from a remote registry; there is no registry URL setting.

### The curated list

`crates/pond-core/src/models/domain/curated.rs` is GIAP's short list, each pick pinned to one
upload (repository, commit, size, sha256):

| Pick | Engine | Repository |
|---|---|---|
| `gemma-4-E4B-it-qat-UD-Q4_K_XL` | llama.cpp | `unsloth/gemma-4-E4B-it-qat-GGUF` |
| `gemma-4-E2B-it-qat-UD-Q4_K_XL` | llama.cpp | `unsloth/gemma-4-E2B-it-qat-GGUF` |
| `gemma-4-E2B-it.litertlm` | LiteRT-LM | `litert-community/gemma-4-E2B-it-litert-lm` |
| `gemma-4-E4B-it.litertlm` | LiteRT-LM | `litert-community/gemma-4-E4B-it-litert-lm` |

The catalogue's conversation rows are exactly these; anything else is added from Hugging Face or
found on disk. Names are the file stems (GGUF) and file names (LiteRT-LM) ponds already assign, so
an assignment survives a change to the list.

`recommended.rs` names three of them, with a reason each and numbers only where they were measured
on that class of machine. Only the REST view reads it
(`crates/pond-core/tests/recommendations_are_never_imposed.rs`): nothing is chosen for the
household.

### Pinning a new pick

1. Read the file's commit and LFS oid: `curl -s https://huggingface.co/api/models/<repo>/tree/<rev>`.
   The pin's `sha256` is `lfs.oid`, **never `xetHash`**. pond-hf-cache names an unpinned blob by
   the download's final ETag, which for Xet-backed files is the xet hash, so a blob's file name
   is not its content hash.
2. Prefer the commit the pick's picture add-on is already pinned at, when the file's oid there is
   the one you verified.
3. Add a `CuratedModel` with `size_bytes`, `sha256`, `context_length` (the model card's
   longest), a short `title`, and `pictures` set to the add-on's pairing `dir` (llama.cpp only).
   The pick's (repo, file) must be listed in the pairing table: a test checks every llama.cpp
   pick has its declared pairing.
4. `cargo test -p pond-core --lib -- curated recommended vision_pairing`, and the catalogue tests
   in `composite_model_catalog_provider.rs`.

## Picture add-ons: the pairing table

`crates/pond-core/data/vision-pairings.jsonl` lists every known model that needs a separate
vision encoder, compiled in and parsed once (`vision_pairing.rs`). It is generated from Hugging
Face by `bash scripts/giap.sh models pairings` (`scripts/models/vision_pairings.py`, sources in
`scripts/models/vision-sources.json`; `--check` compares without writing) and refreshed weekly
through a reviewed pull request opened by `.github/workflows/model-pairings.yml`; the pond never
fetches it. A model with no line reads text only.

A model pairs when its file's base name is listed under any line, or failing that when its GGUF
header's architecture and width match a line's (with the qat flag from the name). When several
publishers list one file name, the generator's publisher preference (unsloth, ggml-org,
bartowski, lmstudio-community) decides, the same way everywhere, so a download and the model's
later use agree.

On a budgeted device (the Orin, or a `POND_DEVICE_PROFILE` emulation) an add-on is offered and
declared only if its `dir` is in `device_budget::DEVICE_MEASURED_VISION`, which ships empty, or on
the runtime vision allowlist. The setting `picture_support_download_enabled` (default off,
Settings: "Allow picture support downloads") puts the Gemma 4 encoders
(`device_budget::GEMMA_VISION_ALLOWLIST`) on that list; `POND_VISION_ALLOWLIST` does the same
from the environment (`1`, `gemma` or `all` for the Gemma list, or comma-separated `dir`s). An
allowlisted model keeps a 16,384-token window with its encoder charged. Off the budgeted device a
paired model always declares pictures.

## Acquisition

`crates/pond-api/src/model_acquisition.rs` plans a download before anything starts and says what
it will fetch:

- `POST /api/v1/models/{category}/{name}/download`: the model file (from its pin when it is a
  pick) and its picture add-on, included by default; `{"pictures": false}` leaves it out.
- `POST /api/v1/models/download/url`: the same planner for a file named by URL; it becomes an
  `added` row under a sanitised file name.
- `POST /api/v1/models/{category}/{name}/companions/pictures`: the add-on alone, for an installed
  model.

Every Hugging Face file goes through `pond-hf-cache` (`crates/pond-hf-cache`), which keeps
`huggingface_hub`'s on-disk layout so Python tools can share it, checks a pinned size and ETag
before writing anything, resumes from `.incomplete`, and links the finished blob into the
category's directory (`model_layout.rs`).

A model file whose size nothing has stated (a file named by URL, typically) is asked for it with
a HEAD chain gated hop by hop before anything is fetched, so the announcement and the parts carry
the number first, and the row keeps it. A host that does not say leaves the number out rather
than guessing it.

Every file goes through the download tracker (`GET /models/download/progress`), whose entries
carry `model_id` and `part` (`model` or `pictures`). One transfer runs per tracker key: a second
request for a file in flight starts nothing and answers `already_downloading`, and a resume of
a running transfer does the same. A running transfer holds a clone of its entry's control flag,
which is how an entry left at `downloading` by a task that died is told from a live one. A
failed entry's `error` says what happened and what to do in plain words
(`crates/pond-api/src/download_failure.rs`); the raw error goes to the log.
`POST /models/download/control` takes `{filename, action}` for one file or `{model_id, action}`
for every part of a model, with `action` one of `pause`, `resume` or `cancel`. Pause keeps a
Hugging Face transfer's partial file to resume from (a transfer from any other host has no range
resume, so its pause discards the partial and a resume starts over); each entry's `resumable` says
which. Cancel deletes the partial, cancelling a model's own file cancels its add-on too, and a
paused entry is never evicted.

A row's `companions` read `downloading` while the add-on comes down, `verifying` while its hash is
checked, then `installed`. When the model file arrives its row is marked downloaded, and when
either part arrives the agent registers the model by the file its row names and verifies and
attaches the add-on (`Agent::prepare_model`). Choosing a model, saving settings, a provider build
or a refused picture turn never fetches a conversation model or a picture add-on.

### Downloads that drop

Hugging Face's CDN closes long transfers early (one 4.2 GB GGUF on the Jetson stopped four times,
at 0.85, 2.36, 2.40 and 3.92 GB). `HfFetch::download_to_blob` retries by itself, on the tracked
dashboard path and on the CLI's `setup` alike:

- **What is retried**: a body that ended short (`TransferError::Short`), a failed or dropped
  connection, and a failed chunk read. Each attempt goes through the same HEAD, then a GET with
  Range from the `.incomplete`.
- **Attempt limit**: eight attempts in a row that bring no new bytes. An attempt that gets
  further resets the count, so a file that drops every gigabyte still arrives.
- **Backoff**: 2 s before the next attempt, doubling up to 60 s.
- **Not retried**: a size or etag refusal, a range the server answered from the wrong byte, an
  overlong body, an HTTP status, a disk error or a network-mode denial.
- **Stalls**: `build_redirect_aware_client` sets a 60 s `read_timeout`, per read, not for the
  whole transfer. A connection that goes silent fails, and the failure is retried.
- **Pause and cancel**: the callback is asked again on every 250 ms poll of the wait, so a pause
  or cancel during the wait stops the download at once.
- **Logs**: each retry logs `kind="download_retry"` with the attempt, bytes on disk and the
  delay; giving up logs `kind="download_gave_up"`.
- **Dashboard**: while a transfer waits, its tracker entry stays `downloading` and carries
  `reconnecting: {attempt, max_attempts}`; the card reads "Reconnecting (attempt n of 8)"
  (`models.download.reconnecting` in `pond-desktop/src/i18n.ts`).
- **Failed entries**: a failure that left a resumable file carries `pond_hf_cache::KeptPartial`,
  recorded as the entry's `partial`. Such an `error` entry is not evicted after five minutes, so
  the card keeps its reason and "Try again"; `cancel` on it deletes the file.

## Companion models

What the pond fetches without a choice in the moment, and why each is safe to fetch:

| What | When | Where it is decided |
|---|---|---|
| An assigned model whose file is missing, and its add-on if it had one | At boot (`restore_assigned_models`), through the tracker. Only roles that load a model (chat, tool, asr, tts, embedding), and only when no other quant of it is on disk | `crates/pond-api/src/model_acquisition.rs` |
| The MTP drafter for a Gemma 4 E2B or E4B chat model (`mtp-gemma-4-E2B-it.gguf`, `mtp-gemma-4-E4B-it.gguf`, about 57 MB each, pinned by size and sha256 to the unsloth qat repositories) | On a budgeted device only, in the background, when the provider is built for that model or the model's file arrives | `pond_core::models::domain::drafter`, `crates/pond-adapters-goose/src/mtp_drafter.rs` |
| Kokoro engine weights and voices, espeak-ng phoneme data | At `serve` and `chat --voice` start | `crates/pond-server/src/model_download.rs` |
| Silero VAD, the ONNX Runtime library | When voice starts | `model_download::ensure_silero_model`, `ensure_onnx_runtime` in `main.rs` |
| fastembed's embedding model | On first use by fastembed (`embedding_provider = "fastembed"`, the default) | fastembed itself |
| The GGUF embedding model (`nomic-embed-text-v1.5` unless `active_embedding_model` names another, about 146 MB, pinned by size and sha256) | At `serve` start with `embedding_provider = "gguf"` when its file is missing (`restore_model`), through the tracker into `<data_dir>/models/embedding/`, so it shows on the Models page. It is also a catalogue row the household can download | `pond_core::models::domain::embedding_models`, `main.rs` |
| Face models | With the `face-onnx` feature, at `serve` start, in the background: the listeners never wait on them, and recognition switches on when they land ([face-recognition.md](../face-recognition.md#startup-never-waits-on-the-download)) | `model_download.rs`, `start_face_recognition` in `main.rs` |
| The vision classifier (YOLOX-Nano, about 4 MB) | With the `vision-onnx` feature and no `vision_classifier_model` set, at `serve` start, before the listeners bind | `model_download.rs` |

The drafter is registered in goose's engine registry (`mtp-gemma-4-E{2,4}B-it`) and pointed at by
every registry row naming the chat model's file; it lives at `<data_dir>/models/gguf/<file>` and the
disk scan never lists it as a model. A drafter that is deleted is dropped from those rows on the
next provider build. The engine loads a drafter on CUDA builds only (see
[inference_optimization.md](inference_optimization.md#speculative-decoding-mtp)), so off the Orin it
is neither fetched nor attached. A fetch through pond-hf-cache, the drafter's included, asks the
egress gate first (`EgressPurpose::Models`), so `network_mode` can refuse it.

## Storage layout

`crates/pond-core/src/models/domain/model_layout.rs` is the one mapping from (category, file) to a
path under `<data_dir>/models`. It keeps only a name's last component. Storage, the routes, the CLI
and disk usage all use it.

| Category | Directory |
|---|---|
| `whisper` | `models/` |
| `gguf` | `models/gguf/` |
| `litert` | `models/litertlm/` |
| `llamafile` | `models/llm/` |
| `tts_kokoro` | `models/kokoro/voices/` (engine weights in `models/kokoro/`) |
| `embedding` | `models/embedding/` |
| `tts_piper` | `models/tts/` (legacy rows only) |
| `ollama`, `tts_http` | no local file |

## Seeding and pruning

Every seed upserts the catalogue and prunes what it leaves stale
(`model_service::apply_catalog`): companion rows, custom rows that lost their file and have no
source, bundled rows no longer bundled and never downloaded, and Ollama rows a running Ollama
server no longer lists. Anything assigned stays, nothing the fetch itself lists is pruned, a row
whose file is on disk stays unless it is a companion (the downloaded flag can lag the disk), and
no file is touched. An Ollama that does not answer, or answers with no models, changes none of
its rows: the fetch cannot tell the two apart, so the last model removed from Ollama leaves its
row behind. Rows are told apart by exact id; `Foo` and `foo` are two rows.

Deleting a model refuses (409) while it has a job, or while another row that names the same file
has one (an older row under another id, or another spelling of the name). The refusal names the
model by its title and the job in the household's words (`ModelRole::job`): Conversation,
Listening, Speaking, Memory, Tool helper.

## Memory budget

`GET /models/memory-status` reports `total_mb`, `available_for_llm_mb`, `budget_mb` and
`reclaimable_mb`. On a budgeted device they are the board's: its fixed total, `MemAvailable`, and
the budget left after the system, speech and voice (`device_budget::LLM_BUDGET_MB`). Any other
machine reports its own total and available memory (`sysinfo`, in the local-inference adapter's
scheduler), with a budget of that total less a desktop reserve
(`device_budget::host_llm_budget_mb`: a quarter of memory, at least 4 GB). Available never exceeds
the budget, and what a switch frees is counted against that same budget, so the Models page's fit
check (`pond-desktop/src/api/modelFit.ts`) and the server's spill warning
(`model_spills_budget` in `crates/pond-api/src/routes.rs`, 1,024 MB headroom on both) read one
status.

## No model chosen

With no conversation model, the chat routes answer `409 {"code": "no_model"}` before anything is
saved, the voice child says so as an NDJSON `error` event, and warm-up and quiet compaction skip.
Nothing is picked or downloaded in its place.
