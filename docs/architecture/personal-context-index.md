# Personal-context index — design and handoff

**Status: 0a–0c and phases A–G landed 2026-08-13. VERIFIED THROUGH THE DEPLOYED BINARY ON THE ORIN
2026-08-17. Still open by design: G's passive prompt tier (a measurement, not a guess) and F's media
captioning (blocked on absent encoder bytes). See §7.**

The line that stood here said "Phases A–G unstarted" for four days after they had all landed, and
the closing paragraph of §7 said it too. Read §7's log and the phase table, not a status line.

A single semantic retrieval surface over the three things this pond knows about a household —
extracted **memories**, ingested **context items**, and conversation **summaries** — so the agent can
find what is relevant instead of being handed a fixed slice of it.

Spans PAI-3 (retrieval), PAI-4 (summaries), PAI-8 (context). Kept in its own document because it
belongs to none of them alone; each phase should be stamped into the PAI ledger as it lands.

---

## 0. Read this before believing anything below

Every current-state claim here was verified on **2026-08-12** by reading the tree, and is cited by
symbol as well as `file:line`. **Grep the symbol.** Line numbers rot, and this programme has a
recorded history of plans built on stale ones.

Two claims in the existing docs were found to be **wrong** while writing this, which is the reason
for the warning:

* PAI-8 P2's phase text says `context_items` are "a second corpus in `<system-context>` with its own
  budget". **There is no prompt path for context items at all.** The only production consumer of
  `ContextItem` is `crates/pond-mcp-server/src/context.rs` — the MCP tools — and
  `ext_context_enabled` ships `false`, so on a default pond the corpus is unreachable by the model.
* The on-device-intelligence skill said image input was "one dropped parameter from working". It is
  wired: `attach_images` (`goose_agent.rs`), and `image_part_count` beside it.

---

## 1. Verified state

### 1.1 The blocker that gates everything

`FastembedEmbeddingProvider` (`crates/pond-infra/src/fastembed_embedding.rs`) is the **only**
production `EmbeddingProvider`. `Settings::embedding_provider` accepts `"fastembed"` or `"none"` and
defaults to `"fastembed"`.

On the Orin it does not work. Measured twice on 2026-08-12:

```
WARN embedding provider failed to init: embedding provider init timed out
     after 30 s — ONNX Runtime may be version-incompatible (need ORT 1.24.2)
```

Retrieval falls back to keyword (`search_recent`, taken when the semantic path finds no embedded
rows). **Consequence: "semantic memory injection" — recorded as landed under PAI-3 Phase A
(`e0c5d905`) — has never run on the hardware GIAP ships to.** True on a Mac, keyword matching on the
pond. This is a live defect independent of this plan, and it makes every phase below inert until
fixed.

### 1.2 The three corpora, as they actually are

| | `ContextItem` | `MemoryFragment` | `sessions.rolling_summary` |
|---|---|---|---|
| Owner | `profile_id: String` | `profile_id: Option<String>` | session's, nullable |
| Redacted before store | **yes** — `from_parts` takes a `&dyn Redactor`, no second constructor | **no** | no |
| Sensitivity | `PrivacySensitivity`, floor-enforced, `Secret` unreachable | **none** | none |
| Redaction record | `findings: Vec<RedactionKind>` | — | — |
| Vector field | `embedding: Option<Vec<f32>>` — **populated by `IngestPipeline`, after redaction** (the 2026-08-12 claim that nothing populated it was stale; corrected 2026-08-13) | `embedding: Option<Vec<f32>>`, `#[serde(skip)]` | — |
| Mutability | re-sync by `external_id` | supersede via consolidation | **overwritten in place** |
| Deletion | source disconnect deletes items | decay/prune | with the session |
| Consolidation | none, and it would be a bug | **already built** | n/a |

`MemoryFragment.profile_id: None` means **shared household context, deliberately, and it outlives the
member** — stated at `sqlite_memory.rs :: count_for_profile` ("Deliberately no `OR profile_id IS
NULL`. See the port doc: those rows are shared household context and they outlive the member.").
That is a policy decision already taken; do not re-litigate it, but note it makes "may this caller
see this row" answerable for context items and *not* for memories.

Memory consolidation already exists and is well thought through: `MemoryLifecycle`
(`Active | Archived | Merged`), `superseded_by`, `MemoryTier` (`Short | Long | Permanent`),
`decay_rate`, `access_count`, `last_accessed_at`, a `MemoryEdge` DAG with `EdgeRelation`, and
`corrects` — which exists specifically "so consolidation never accidentally reverts the fix". Reuse
it; do not build a second one.

**Context items must never be consolidated.** They mirror an external system keyed by `external_id`.
Merging two would destroy the idempotency key and the next sync would recreate them — consolidation
fighting the connector forever. Context is a mirror; memory is a workspace.

### 1.3 Summaries already exist and are already curated

`sessions.rolling_summary` + `rolling_summary_through_id` + `rolling_summary_updated_at`, added by
`migrations/system/0030_session_rolling_summary.sql`. Refreshed by `SessionSummaryService` on an idle
path that is cancelled by a new turn and that no turn waits on. Used for exactly one thing today:
splicing into **that same session's** history for compaction.

The design premise here was "every conversation has a curated summary that is unreachable from any
other session", so indexing them is nearly free value. **Measured on the Orin 2026-08-13, that premise
is false on real usage**: 636 sessions, 2 153 messages, and **exactly ONE rolling summary**.

It is not a bug. `refresh` needs `len - KEEP_RECENT_MESSAGES(6) >= MIN_UNSUMMARIZED_MESSAGES(4)`, so a
session needs **10+ messages** before it can ever produce one — and this pond's usage is short bursts:
380 sessions of exactly 2 messages, only 32 sessions at 10 or more. So **604 of 636 sessions are
structurally incapable of ever having a summary**, and of the 32 that could, one does.

Consequence for the phases below, and it is a reprioritisation rather than a blocker: the summary
corpus is a rounding error next to memories and context items. Phase B should still write summaries
through (it costs one embedding on a refresh that already happened), but **C and G must not be
designed assuming summaries carry retrieval weight on a real device** — on this one they would
contribute a single row.

**Checked 2026-08-13, and the worry was misplaced.** Two things write `sessions.rolling_summary`, and
only one is tier-gated. `SessionSummaryService::refresh` — the incremental fold of "previous summary +
the messages it does not cover" — **runs on every tier**, and `resummarise` (PAI-4 P2's rebuild from
source) is the `ModelClass::Large`-only one. The idle loop that drives `refresh` is wired in
`main.rs` with a real `LlmProvider`, so an on-device pond does produce summaries.

Two caveats for phase B rather than blockers: a refresh costs a LOCAL LLM call at idle on the Orin
(not free, though no turn waits on it), and it only fires past `summary_idle_secs` with at least
`MIN_UNSUMMARIZED_MESSAGES` beyond the through-pointer and outside the recent tail — so a pond used in
short bursts may hold few summaries. **The device half of 0c is still owed**: read `rolling_summary`
out of `sessions` on the nano and confirm rows exist in practice, not just in principle.

### 1.4 No vector extension, and that is fine

No `sqlite-vec`, `sqlite-vss`, `vectorlite`, `usearch`, `hnsw` anywhere in the dependency tree, and
nothing calls `load_extension`. Similarity is brute-force cosine in Rust, in three separate copies:
`sqlite_memory.rs :: cosine_similarity`, `sqlite_context.rs :: cosine`,
`sqlite_face_recognition.rs :: cosine_similarity`.

At household scale (thousands of rows) brute force over 384-dim f32 is sub-millisecond and
irrelevant beside a flat ~30 tok/s decode. **Do not add a compiled extension**: it buys nothing here
and adds a native dependency to a build already fighting ONNX Runtime, llama.cpp, candle and
aarch64 cross-compilation. The bottleneck is the embedder.

### 1.5 WAL, and why it shapes the design

`db.rs` sets `journal_mode = WAL` and **asserts** it on both pools. SQLite cross-database
transactions are **not atomic in WAL mode**, so a context item and its vector in a second file
cannot be deleted atomically. Everything in §2 about "no text in the index" follows from this one
fact.

### 1.6 What the ingest producer already refuses, and why it is right

`crates/pond-core/src/context/producer.rs` states the rule this plan should not weaken:
**keep transitions, drop samples.**

* `DISCRETE_SENSOR_TYPES` = `motion`, `occupancy`, `contact` (+ bridge aliases). Everything else is
  dropped. An unrecognised signal produces nothing until somebody adds it — the narrowing direction.
* Continuous readings are refused because the pond **already** stores them in `sensor_readings` under
  `retention_sensor_days` with min/max/avg behind the `giap-sensor` tools. Copying the series in
  "buys nothing and costs the budget twice", and at 2 880 rows/day/sensor would drown the corpus.
* The rule is about the **signal, never the value**, and that is not laziness: `pond-adapters-matter`
  maps Matter `BooleanState` with `true = closed`, so a producer keeping non-zero readings would file
  "the door is shut" as news and drop "the door opened".
* Voice is refused **by type**: `NotIngested::VoiceIsCuratedByMemoryExtraction`.

### 1.7 On-device numbers to design against (Orin, 2026-08-12)

| Quantity | Value |
|---|---|
| Cold turn prompt | 7 568 tokens |
| Tool schemas | ~88% of the preamble |
| Per tool round-trip | ~670 tokens |
| Prefill | 674–976 tok/s, falling with depth |
| Decode | flat ~30 tok/s at every depth |
| `LOCAL_PROMPT_CLAMP` | 8 192 (prompt-side, all local providers) |
| Compact static prefix cap | 2 400 chars ≈ 600 tokens (`v2_compact_static_prefix_within_token_budget`) |
| E2B / E4B KV | 18 / 56 KiB per token |

Existing backfill precedent: `main.rs`, batched with a pause between batches, carrying the note "on a
Jetson this competes with inference for CPU". Respect that.

### 1.8 Media

Image input is wired (`attach_images`) and `SourceKind::Camera` exists. **Corrected 2026-09-24:**
the claim this section used to make, "`find ~/.local/share/goose-in-a-pond/mmproj -type f` on the
nano returns nothing", looked in the wrong place: encoders live under `<data_dir>/models/mmproj/`.
Measured there, read-only: the E4B encoder is complete (991,552,320 bytes, the NON-qat file), and the
E2B encoder is TRUNCATED at 636,790,074 of 986,833,728 bytes, the "truncated file" recorded earlier,
which was counted as ready and failed at every E2B load. The Orin's active model,
`gemma-4-E4B-it-qat-UD-Q4_K_XL`, could not take a picture at all: the lookup never resolved a qat
name, and the qat encoder is a different file from the non-qat one. Phase F6 of the context roadmap
replaced the whole path. The Orin now declares no picture support until an encoder is measured
there (`device_budget::DEVICE_MEASURED_VISION`), so media captioning on the nano waits on that
measurement, not on missing bytes.

---

## 2. Decisions already taken

Taken deliberately, with the reason. Changing one is allowed; changing it silently is not.

| Decision | Reason |
|---|---|
| **One index, separate stores** | An index is rebuildable; a merged store cannot be unmerged. The three corpora have genuinely different lifecycles and guarantees |
| **Its own database, `pond_vectors.db`** | Derived data, rebuildable, keeps any future native extension out of the authoritative DB's blast radius, and never syncs to a phone |
| **`ATTACH` for reads, separate writes** | Gives real `JOIN`s so scope and sensitivity are filtered against live rows — no denormalised copies to go stale. Only cross-file *transactions* lose atomicity in WAL; queries are fine |
| **The index holds NO text** | Under WAL a delete cannot be atomic across files, so an orphan is inevitable. With no text an orphan is harmless noise that resolves to nothing and drops out. With a snippet it is deleted data that survived a deletion promise |
| **`model_id` column** | A vector from a different embedder still scores plausibly and is wrong. Mismatch must be detectable, not silent |
| **`corpus` tag** (`memory`/`context`/`summary`) | Lets them share retrieval without sharing guarantees, and lets retrieval label provenance |
| **No queue — staleness is a `LEFT JOIN`** | Crash-safe, restartable, self-healing, and the file can be deleted and rebuilt. A durable queue fails silently: a dropped entry is an item never searchable with nothing to notice |
| **Backfill deferred to idle, not startup** | A household's first turn after an upgrade must not be slow because the pond is indexing |
| **No faces in this index** (Jerry, 2026-08-12) | Biometrics have their own consent and deletion story; "delete my face data" must not be a query against a table holding calendar vectors |
| **Tool surface before passive prompt tier** | A tool costs nothing on turns that do not use it; a prompt block costs tokens on every turn |
| **Summaries: upsert + re-embed, labelled, memory wins ties** | A rolling summary is overwritten in place, so an append leaves a vector describing an older conversation. It is a model's compression, not a claim, so it must be labelled and must not outrank the precise version of the same thing |
| **Media: eager for deliberate shares, lazy for libraries** | You cannot lazily caption a poster you do not know is a poster; but a library sync is hours-to-days of GPU and must stay on-demand |
| **Derived facts cascade, approved actions do not** | Delete a photo and its caption goes. A reminder the member *approved* is their own commitment, not provenance-bound data |

### Open decisions

1. **Embedding dimension — a one-way door. RESOLVED 2026-08-13: 768, via
   `nomic-embed-text-v1.5` (mean-pooled).** Retrieval-tuned, among the best-supported embedding
   GGUFs in llama.cpp (so most likely to init on the Orin — the real gate), and its task prefixes fit
   the query-vs-memory asymmetry. At household scale 768-dim brute-force cosine is still sub-ms. The
   code derives nothing from a hardcoded dimension it cannot check: `dimensions()` returns the spec's
   declared width, `load_sync` refuses the model if `n_embd` disagrees, and every vector will carry
   `model_id = "nomic-embed-text-v1.5"`. The declared fallback is `bge-small-en-v1.5` (384, CLS) —
   swapping the default to it is one line **only while no 768-dim vectors have been stored**, which is
   the whole meaning of "one-way door". Confirm nomic loads on the Orin before the first store; if it
   does not, switch the default then, not after. (`pond_inference::EmbeddingModelSpec`.)
2. **Mail: subjects only, or bodies too?** Recommendation is subjects + sender + date. Bodies change
   the volume and the exposure enough to be their own phase.
3. **Does a Guest see shared household memories?** `profile_id: None` is household-visible by the
   port's own rule; whether that reaches a `Guest` session is a privacy call, not a technical one.
   Context items already answer it: a Guest sees none.

---

## 3. What to stream in

| Source | Content | Rate | Status |
|---|---|---|---|
| Sensor | state transitions only | few/day | **landed** |
| Camera | classified events above a confidence floor | few/day | **landed** |
| Voice | nothing — refused by type | — | **landed as a refusal** |
| Media (deliberate share) | poster/receipt/screenshot + extracted intent | few/day | new |
| Calendar | title, time, place, participants | ~10s/week | connector phase |
| Mobile (GOTG) | place arrivals/departures, **not** a GPS track | transitions | connector phase |
| Mail | subject + sender + date | 100s/week | connector phase |
| Files | title + summary, not contents | bounded | connector phase |
| Chat | nothing by default; per-channel opt-in | — | connector phase |

**Deliberately excluded**: raw transcripts, mail bodies, GPS tracks, continuous series of any kind,
group chat by default, and anything principally about a non-member.

**The test before adding a source:** would a member recognise this row as *something that happened*,
six months from now? Transitions pass. Samples do not. Conversation does not — that is memory's job.

This test also predicts volume, which is what actually kills a corpus on this hardware. A few items a
day per source keeps a household in the thousands of rows — brute-force territory, no extension
needed. Admit mail bodies and GPS tracks and it is millions, and §1.4 stops being true.

---

## 4. Phases

| Phase | Deliverable | Verified by |
|---|---|---|
| **0a** | GGUF `EmbeddingProvider` that initialises on the Orin | **MODEL VERIFIED ON THE ORIN 2026-08-13**: `nomic-embed-text-v1.5.Q8_0` loads under the device's own CUDA llama.cpp (aarch64, `ARCHS = 870`), mean-pools, returns **768** dims, exit 0, in **0.51 s wall-clock including cold process start and model load**. The risk that killed fastembed is closed. Still owed: the same through a deployed GIAP binary with `embedding_provider = "gguf"` |
| **0b** | ~~`ContextItem.embedding` populated in `IngestPipeline` — **after** redaction~~ **ALREADY LANDED**, found 2026-08-13: `ingest.rs` embeds `item.embedding_text()` after `from_parts` has redacted, and `main.rs` wires `.with_embedder(embedding_provider)`. What was missing was the GUARD — `the_vector_is_computed_from_the_redacted_text` now records what the embedder was handed, because the existing redaction test would stay green if the embed moved above it | a stored item has a vector; the vector is of redacted text — **both now pinned, mutation-tested** |
| **0c** | Confirm `rolling_summary` is produced on-device | **code half done 2026-08-13**: `refresh` is NOT `Large`-gated (only `resummarise` is) and its idle loop is wired with a real provider, so it runs on any tier. **Device half DONE 2026-08-13**: 1 summary across 636 sessions — the mechanism works and the corpus is nearly empty; see 1.3 |
| **A** | `pond_vectors.db`, port + adapter, `ATTACH` on `after_connect`, migrations | **LANDED 2026-08-13.** `VectorIndex` port (`context::vector_index`) + `SqliteVectorIndex`; migration `vectors/0001`; `Database::vectors` opened after the system migrations. Verified: roundtrip; **the file is deleted and rebuilds**, reporting its source rows as needing embedding; an orphan matches nothing and prunes; a foreign-model vector is excluded rather than scored; a guest sees nothing and an owner sees own + unattributed. Live: fresh pond creates and migrates it, restart against a populated one preserves rows and re-runs nothing |
| **B** | Write-through for all three corpora | **LANDED 2026-08-13.** Memory + context mirror their EXISTING vector at the SQLite adapter (zero extra inference); summaries are a sweep (`summary_indexing`) because nothing had ever embedded them. Verified live: a memory written through the API is embedded by the backfill and appears in `pond_vectors.db` stamped `nomic-embed-text-v1.5`/768, and a real query ranks it **0.66 vs 0.45** above a decoy sharing the word "spend" — the semantic claim, on a running pond. A re-summarised session is reported stale and its vector replaced |
| **C** | Unified retrieval, scope in the SQL, `corpus` labelling | **LANDED 2026-08-13.** `PersonalContextRetrieval` (pond-core, policy) over `VectorIndex::search_resolved` (adapter, mechanism): one query answered across all three corpora, text read from LIVE rows in the same JOIN that filters scope and liveness. `three_way_isolation_two_members_and_a_guest` passes, plus guards for archived rows, unattributed (guest) summaries, and memory-wins-ties |
| **D** | Idle staleness sweep, orphan prune, model-change re-embed | **LANDED 2026-08-13.** `run_index_maintenance` composes adopt -> embed-missing -> prune -> report, deferred 60 s from boot and cancellable. Live: index wiped and an orphan planted, 5 memories adopted and the orphan pruned ~45 s later; a model mismatch WARNs with its count; an unattributed (guest) session's summary is refused and indexed only once attributed |
| **E** | Trigger subscribers (`BusEvent`) | **SATISFIED 2026-08-13 WITHOUT A NEW SUBSCRIBER.** A sensor/camera event already flows `BusIngest::absorb` -> `IngestPipeline::ingest` -> `save_item` -> the phase-B write-through, ending at the same terminal write. A second subscriber would be a parallel path to the same row with its own way of going wrong. Pinned end-to-end through the real pipeline; disconnecting a source now also removes its vectors |
| **F** | On-demand route/tool + lazy media caption | **TOOL LANDED 2026-08-13** (`giap-context__recall`). **Media captioning DEFERRED** and not started: it needs picture support on the nano, which waits on an Orin measurement (1.8; the earlier "mmproj bytes are absent" reason was a wrong path) |
| **G** | Retrieval surfaces: `giap-context` tool first, then a **measured** passive prompt tier | **TOOL HALF LANDED 2026-08-13**, which is the half the design says comes first. The passive prompt tier is **NOT DONE and deliberately not guessed**: whether an always-on block earns its tokens against a 7 568-token cold turn is a `pai-bench` measurement on the Orin |

**G is last and empirical on purpose.** Whether an always-on prompt block earns its tokens against a
7 568-token cold turn is a measurement, not an opinion.

### The four indexing modes

| Mode | Trigger | Notes |
|---|---|---|
| **Automatic** | write-through on the normal paths | embed **after** redaction; summaries **upsert** |
| **Trigger** | `BusEvent::Ingest` (new variant), `Camera`, `Session` idle, source disconnect | second bus subscriber beside `BusIngest` |
| **On-demand** | explicit route/tool; lazy media captioning | bounded per query — a question must not cause a five-minute stall |
| **Schedule** | idle-gated sweep: staleness, orphans, model-change re-embed, retention reconcile | batched + paused, cancelled by user activity |

**Model change must be loud.** Every stored vector becomes meaningless while still scoring
plausibly. On mismatch: `WARN` with both ids and the row count; retrieval prefers matching vectors
and falls back to keyword for the rest rather than mixing spaces; re-embed runs idle-gated with
progress and may take hours on a Jetson. "Retrieval quietly got worse" is undiagnosable.

---

## 5. Failure modes and the guard each needs

| Failure | Guard |
|---|---|
| Vector survives its item's deletion | no text in the index; orphan resolves to nothing |
| Scope filter forgotten → cross-member leak | scope in the `JOIN`'s `WHERE`, never post-filtered |
| Post-filtering degrades retrieval | top-K slots consumed by invisible rows; filter first |
| Summary vector describes an old conversation | upsert + re-embed on write; assert a second summary changes the vector |
| Mixed embedding spaces | `model_id`; a mismatch is refused, not scored |
| Sweep starves inference | idle-gated + batch pause; assert user activity cancels it |
| Index silently incomplete | staleness is a query — expose the count on a health route |
| Guest sees household rows | explicit test; zero from every corpus |
| Vector of un-redacted text | embed after redaction; a vector is not auditable |

---

## 6. Uncommitted work in the tree at handoff

`crates/pond-core/src/prompts.rs` and
`crates/pond-core/src/models/services/prompt_builder.rs` — ~171 lines, **not committed**, gates green.

Three parts, and they should not all land:

1. **Identity rewrite, all four styles** — "personal agentic assistant", whose pond it is, tools as
   live connections. **Measured good**: E4B went from 2 tool calls to 7 on a fan-out query, producing
   a real per-state answer where it had refused. Worth keeping.
2. **The "never narrate the harness" rule** — **measured bad.** Both probe answers still leaked
   (`"the goal has not been fully met"`, `"I cannot continue working toward this goal"`). Asking a 4B
   model to be discreet about its own input does not hold. Recommend **dropping it** rather than
   shipping a prompt instruction that costs tokens on every turn and does not work.
3. **`resolve_builtin_template_selects_correct_style` rewritten** to compare against the constants
   instead of matching identity prose. Needed either way — the old version breaks on any wording
   change, which is how it broke here.

**The real fix for the leak is not a prohibition.** Reword the nudge in the goose fork patch so it is
quotable: "Have you fully answered what was asked? If not, keep working." A model that echoes *that*
produces a sentence a user can read. Two lines, and robust where an instruction is not.

---

## 7. Progress log

### 2026-08-13 — Blocker 0a: GGUF `EmbeddingProvider` implemented and Mac-verified. Orin run owed.

**What landed** (`crates/pond-inference/src/embedding.rs`, wired in `pond-server/src/main.rs`):
`GgufEmbeddingProvider` implements the `EmbeddingProvider` port over llama.cpp
(`llama-cpp-2 =0.1.146`, whose embeddings API — `with_embeddings`, `with_pooling_type`,
`embeddings_seq_ith`, `n_embd` — is present at that pin). It lives in `pond-inference` because that
crate already owns the goose-coexisting backend singleton, the model loader and the exact
metal/cuda feature wiring, and depends on `pond-core`. Selected by `embedding_provider = "gguf"`,
which the default build reaches because `local-inference` now pulls `pond-inference` (no new native
compile — llama.cpp is already built by `goose-agent` and `local-inference`). The model is fetched
once through `HttpModelDownloader` → `model_download::download_file`, which calls `egress::begin`, so
the download is `network_mode`-gated (invariant 4).

**Dimension decided: 768, `nomic-embed-text-v1.5`, mean-pooled.** See §2 open-decision 1. Not
hardcoded anywhere it cannot be checked: `dimensions()` returns the spec width, `load_sync` refuses a
model whose `n_embd` disagrees, and the spec carries the `model_id` stamp §2 requires. Fallback
`bge-small-en-v1.5` (384) is a one-line default change **only before the first 768-dim store**.

**The hazard §4/the phase table did not name, and it is a panic — REPRODUCED ON A MAC 2026-08-13.**
llama-cpp-2 guards backend init with a process-global flag, and Goose's own local-inference treats a
second `LlamaBackend::init()` as `unreachable!`
(`goose/crates/goose-local-inference/src/llamacpp/mod.rs`, the `BackendAlreadyInitialized` arm).

**The lazy-load mitigation this entry originally claimed was sufficient is NOT.** That claim — "the
model loads on first `embed()`, after a chat turn, so Goose always goes first" — is false twice over.
`main.rs` spawns a memory **backfill** as soon as the provider exists, so the first embed happens at
STARTUP, not after a turn; and a pond whose `chat_provider` is not local at boot never initialises
Goose's backend at all, so the embedder wins the race whatever the ordering. Reproduction:

```
chat_provider=ollama + embedding_provider=gguf + one unembedded memory
  -> backfill embeds at startup, embedder calls LlamaBackend::init() and WINS
  -> switch chat_provider to local
  -> thread 'tokio-rt-worker' panicked at goose-local-inference/src/llamacpp/mod.rs:355:17:
     internal error: entered unreachable code: the runtime holds the only LlamaBackend
```

The process survived the panic (it is on a worker task) but the API stopped answering, and local
inference is dead for the life of the process. **Ordering cannot fix this from the pond side** — any
embed claims the backend, and the provider switch can happen at any time — so moving the backfill
later would have been cosmetic.

**FIXED 2026-08-13, and with NO goose patch — the patch set stays at 6.** The insight is that the
`AtomicBool` is llama-cpp-2's *Rust-side* bookkeeping, not llama.cpp's: the C `llama_backend_init()`
is idempotent (this crate already relied on that), and `LlamaBackend` is a public field-less struct,
so the proof-of-initialisation token can be constructed safely without `mem::zeroed()`.
`engine.rs :: get_or_init_backend` therefore initialises the C backend directly and **never enters the
CAS**, so the flag is only ever set by Goose, whose init always succeeds. This does not fight Goose's
stated invariant ("the runtime holds the only LlamaBackend for the life of the process") — **it makes
it true again.**

The handle is also now held as a strong `Arc` in a `OnceLock` rather than a `Weak`, because
`impl Drop for LlamaBackend` resets that global flag *and* calls `llama_backend_free()`: with two
consumers, whoever drops first frees the backend under the other and the second dropper panics inside
a destructor. Once the backend is shared the only sound rule is **initialise once, never free** —
which also removes the ggml teardown race the old `Weak` comment was worried about.

Verified on the Mac, both orderings, zero panics: the exact previously-panicking sequence
(`ollama` boot -> embed at startup -> switch to a local model) now returns 200 and leaves the API
healthy; and with a local model at boot, a **real chat turn** (gemma-4-E2B, 7 212 prompt tokens,
`model_load_ms=2108`) completes in the same process as a loaded embedding model. Two source tripwires
guard the property — `no_giap_code_calls_llama_backend_init` and
`the_backend_handle_is_held_strongly_and_never_freed` — both mutation-tested.

**The second hazard, and it is the one that would have shipped silently: MIXED VECTOR SPACES.**
Introducing a second provider introduces a second WIDTH, and §5 lists this failure mode with a
`model_id` column as its guard — a column that belongs to a later phase and does not exist. What a
384/768 pond actually did, before this change: **nothing panicked, nothing warned, and every
comparison returned exactly `0.0`**, because every similarity function guards `a.len() != b.len()`
and returns 0.0 — a *valid score*, not an error. The consequences compounded:

* `sqlite_memory :: search_similar` has **no `ORDER BY`** and checks `rows.is_empty()` *before*
  scoring, so a full page of incomparable rows suppressed the recency fallback and was returned
  ranked as if judged — an arbitrary subset presented as relevance.
* `recall_memories` / `search_context` gate their keyword fallback on non-emptiness, so it never fired.
* `topical_memories` yielded `Some(0.0)` rather than `None`, quietly reverting prompt-time injection
  to importance+recency.
* Semantic dedup (threshold 0.92) silently stopped deduplicating.
* `run_backfill` selects `embedding IS NULL`, so a stale-width vector is **never** re-embedded: the
  degradation is permanent.

Fixed here by filtering incomparable vectors out of the **candidate set** in both adapters (which
makes the existing `is_empty()` fallbacks correct for free) and mapping them to `None` in
`topical_memories`. Both guards were **mutation-tested**: the first versions passed with the fix
removed and were rewritten until they failed. The re-embed path and the `model_id` column remain owed.

**And the fallback model I had documented was itself the trap.** The original entry named
`bge-small-en-v1.5` (384) as the one-line fallback — the same width fastembed emits. Since the width
is the *only* discriminator available, that would have put two genuinely different spaces at one width
where nothing could tell them apart: strictly worse than the mismatch being guarded. The fallback is
now `bge-base-en-v1.5` (**768**), and `no_gguf_model_shares_a_width_with_the_fastembed_provider`
fails the build if any GGUF model is ever added at 384.

**Verification.** `cargo fmt` clean; `cargo test -p pond-core -p pond-infra -p pond-inference` green
(1235 + 308 + module tests); `cargo check -p pond-server -p pond-adapters-goose` green; 326 frontend
tests green. A live embed on the Mac (Metal) passes. **And a real pond-server run on the Mac**: with
`embedding_provider = "gguf"` the server downloads the model, reports
`GGUF embedding provider ready dims=768`, and the startup backfill embedded a seeded row at **768
dims** — the first time this pond has produced a real semantic vector through the live server.

**NOT run on the Orin** — the device was offline (`No route to host` on `nano.local`). This is
`LANDED`, not `VERIFIED`.

**Orin session 2026-08-13 (the device came back mid-session).** Two things settled on real hardware.

*0a's core risk is closed.* `nomic-embed-text-v1.5.Q8_0` loads and embeds under the Jetson's own
CUDA llama.cpp build — aarch64, `CUDA : ARCHS = 870`, NEON/DOTPROD, `-ngl 0` as this provider
configures it — returning 768 dims in 0.51 s wall-clock *including* cold process start and model
load. Against a flat ~30 tok/s decode, an embed is free. This is the question that killed fastembed
and it is answered: llama.cpp starts there, ONNX Runtime does not. What is still owed is narrower
than it was — the same path through a DEPLOYED GIAP binary, which needs a build on the device.

*0c is done and it reprioritises phase B.* See 1.3: one summary across 636 sessions, because 604 of
them are too short to ever qualify. The mechanism is fine; the corpus is not there.

**Phase A landed 2026-08-13.** The index is its own file, `pond_vectors.db`, holding **no text** — under
WAL a source row and its vector cannot be deleted atomically, so orphans are inevitable, and an orphan
that held a snippet would be deleted data surviving a deletion promise. With no text it resolves to
nothing on the join and drops out. Reads `ATTACH` the system database as `sys` on **every pooled
connection** (`after_connect`, not once — an `ATTACH` is per-connection, and doing it once would work
until the pool opened its second one), so scope, lifecycle and existence are filtered against LIVE rows
in the SQL rather than against denormalised copies.

Two things worth carrying. The scope guard was **vacuous on the first attempt**: `search` early-returns
on `excludes_everything()` before any query runs, so opening the SQL predicate up left the behavioural
test green. It is now pinned directly as well, so each layer is guarded by something rather than each
being assumed by the other. And the fixture that invented member ids hit a real FK to `profiles` —
`foreign_keys(true)` doing its job.

`cosine` here returns `Option`, refusing incomparable widths rather than answering `0.0`. That is the
same defect the memory store had: `0.0` is a legitimate score, so using it for "not comparable" fills
result slots with rows that were never judged and suppresses the fallback that should have run.

**Phase B landed 2026-08-13.** Write-through sits in the SQLite **adapters**, not in a decorator, and
the reason is the sharpest thing the mapping turned up: `RedactingMemoryRepository::add` sets
`fragment.embedding = None` when it finds a secret, precisely because the vector is a durable
derivative of the unredacted text. An index decorator stacked ABOVE it would index exactly that
vector. At the adapter the fragment has already been through the redactor, and `add`/`update_embedding`
are a true 100% chokepoint — nothing else in the workspace issues SQL against `memory_fragments`.
Two supporting facts: eighteen of the port's twenty-two methods have default bodies, so a decorator
that forgets one silently no-ops; and one of the four construction sites (`pond memories add`) is a
SEPARATE PROCESS, so anything hung off the server's `AppState` would miss it.

**Memory and context cost nothing.** Both already hold a vector when they are stored, so the
write-through mirrors what is in hand. **Summaries are the only new inference** — nothing had ever
embedded `sessions.rolling_summary` — and they are a SWEEP rather than a write-through, because both
writers are already LLM calls and one runs on the compaction path.

Two bugs caught before they shipped, both by mutation. A vector that cannot be attributed to a model
must be LEFT ALONE, not removed: the CLI has no embedder, and removing would have stripped entries the
server wrote correctly. And the staleness clause needed `v.source_rev IS NOT NULL` — without it a
vector stored without a revision compares unequal to a non-NULL column and is reported stale on every
sweep **forever**, re-embedding the same summaries indefinitely on the one device where inference is
scarce. That guard passed a mutation until a test was written specifically for it.

Observed while live-testing, pre-existing and worth its own fix: `POST /api/v1/memories` constructs
`embedding: None` and never consults the embedder, so a memory written through the API is not
searchable until the next restart runs the backfill. The backfill is startup-only.

**Exercised A and B against realistic use cases 2026-08-13, and it found a real bug.** Six scenarios on
a live pond seeded as a household (two members, shared rows, six memories, one summarised session):

| Use case | Result |
|---|---|
| Cross-corpus retrieval | correct top hit on all five queries; "what did we decide about the garden?" returns the **SUMMARY** (0.67) above every memory — the case 1.3 called an answer with no path to it |
| Member isolation | `jerry` sees own + shared, `sam` sees own + shared, **guest sees nothing**, household sees all seven. No cross-member leak |
| Model change | a vector restamped to another model is **excluded, not scored** |
| Deletion / orphan | source row deleted, vector left: the JOIN drops it |
| Scale | 2 007 vectors scanned and ranked in **114 ms in PYTHON** (Rust is far faster), correct hit still first — the brute-force decision holds |
| **Rebuild** | **FAILED.** Deleting `pond_vectors.db` restored the summary and NOT the five memories |

That last one is the bug, and it is the property the whole separate-file design rests on. The memory
and context sweeps are driven by the per-store `embedding` column being NULL, so an already-embedded
row never reaches the write-through again: delete the index and those rows are absent from it
**forever**, silently, while the store looks perfectly healthy. Only summaries recovered, because that
sweep is index-aware.

Fixed by `VectorIndex::backfill_from_source` — pure SQL across the ATTACH, copying vectors that
already exist into the index with **no inference at all**. Re-tested: five memories adopted, retrieval
identical after the rebuild. It filters on width, because a stored vector of another width came from
another model and must not be restamped into the live space where nothing could detect it (mutation-
tested). Two regression tests pin both halves.

**Phase C landed 2026-08-13, in two parts.** Part one was correctness, because C is where the index
starts answering questions and reviewing its SQL against the write-site map found three defects I had
shipped in A and B: `search` had no liveness predicate at all (archived memories were retrievable
through the index while every direct read of the store hid them); the Summary corpus had no owner
predicate, so a GUEST session's summary — the idle loop summarises every session and takes no scope —
would surface to a household read; and I had written a comment asserting `sessions` has no owner
column, which is false (migration 0003). `sessions.profile_id IS NULL` means "nobody was identified
here", the OPPOSITE of `memory_fragments.profile_id IS NULL` meaning shared household context: same
column name, inverted meaning, and conflating them is a privacy hole. Liveness is now one function
used by all four query paths, since a corpus filtered in one and not another is exactly how it
happened.

Part two is the retrieval surface. `PersonalContextRetrieval` owns POLICY — a query is embedded with
`embed_query` rather than as a document, ties break toward the more precise corpus, every hit carries
provenance — and `VectorIndex::search_resolved` owns MECHANISM, reading each hit's text from the live
row in the same JOIN that already filters scope and liveness. One round trip, and no window in which a
row could be archived between being scored and being read.

**"Memory wins ties" is the declaration order of `Corpus`**, with `Ord` derived, so the policy lives at
the type rather than in a comparator somebody can quietly reorder — and a test pins it, because
reordering those variants changes what the assistant prefers to tell a member. Provenance is prose
("you told me", "observed by this pond", "from an earlier conversation") rather than the enum name,
because it is written for whoever reads the answer.

A failed recall returns empty rather than erroring: the caller's fallback is always better than
failing the turn that asked.

**Phase D landed 2026-08-13.** One composed pass rather than the three ad-hoc startup spawns B left
behind: adopt existing vectors (free, pure SQL), embed the summaries that have none (the only step
costing inference), prune orphans, then REPORT. The order is asserted, because pruning before adopting
would delete rows adoption is about to legitimately re-create.

It is **deferred 60 s from boot and cancellable**, which is the design's own rule finally honoured —
"backfill deferred to idle, not startup", because a household's first turn after an upgrade must not
be slow because the pond chose that moment to index itself.

Verified live by breaking the index on purpose: deleted `pond_vectors.db`, planted an orphan, and
watched a boot show **0 vectors** (the deferral working, where before it indexed immediately) and then
repair itself ~45 s later — five memories adopted with no inference, the orphan pruned. A model
mismatch WARNs with its count rather than degrading silently.

One behaviour worth recording because it looked like a bug and was not: the live pond reported
`summaries=0`. Its session had `profile_id IS NULL` — an unattributed, i.e. guest, session — which
phase C's rule refuses. Attributing it made the sweep index it on the next pass. The rule is doing
exactly what it should, and the sweep will not spend inference on a row retrieval would always refuse.

**E, F and G, 2026-08-13 — two landed, one deliberately not.**

**E needed no new machinery, and that is the finding.** The design imagined a second bus subscriber
beside `BusIngest`. But a sensor or camera event already reaches the index through the chain phase B
instrumented, so a second subscriber would be a parallel path to the same row carrying its own bugs.
Pinned end to end through the real pipeline instead. One genuine gap closed while there:
`disconnect_source` deleted a source's items and left their vectors behind. Harmless — the index holds
no text and the JOIN drops an orphan — but a member disconnecting a source is asking for their data to
be gone, and "eventually, at the next sweep" is a poor answer.

**F/G's tool half landed**: `giap-context__recall` answers one question across all three corpora, each
line stating its provenance. Two guards fired while adding it, both worth keeping. The extension pins
its own tool surface as READ-ONLY, because a write tool there is a prompt-injection path into a corpus
the assistant treats as fact — `recall` only reads, so the list was extended rather than the rule bent.
And a new guard pins that `spawn_context_server` actually hands the server its retrieval: a tool that
is registered, offered to the model and permanently answers nothing is worse than an absent one, since
it burns a schema in every turn's prompt and teaches the model that asking is pointless. That is the
reader-with-no-writer shape this programme keeps recording; it is mutation-tested.

**Note the tool ships behind `ext_context_enabled`, which is OFF by default.** That is the design's own
decision, not an oversight: two tool schemas in every turn are real tokens on a 4 096-token window, and
a pond with no sources can only answer "nothing found".

**G's passive prompt tier is NOT done, and I am not going to guess it.** Whether an always-on context
block earns its tokens against a 7 568-token cold turn is exactly the measurement the phase table calls
for, on the Orin, via `pai-bench`. The tool surface comes first precisely because a tool costs nothing
on turns that do not use it while a prompt block costs every turn.

**Media captioning (F's other half) is not started**, and on the nano it is blocked on what 1.8
records: no picture support is declared on the Orin until an encoder has been measured resident
there. The "no encoder bytes" reason recorded here before 2026-09-24 was a wrong path.

**Probed the whole chain against the live agent 2026-08-14** (`scripts/context_recall_probe.py`).
It plants one fact per corpus and asks the running assistant a question whose answer is only
obtainable from that fact. The design is what makes it informative: memories already reach the prompt
through the OLD path (`topical_memories` injects them directly), so a memory is a CONTROL; context
items and summaries are on no prompt path at all, so answering one of those is only possible via the
`recall` tool over the index.

It found a real defect immediately. **Context items had no embedding path whatsoever.** Adoption only
copies vectors that already exist, and nothing ever embedded an item that arrived without one, so the
entire corpus was silently unsearchable — the planted sensor event was never indexed and the
assistant answered "there is no recorded activity", which is a WRONG answer rather than a missing
one. Fixed with `needs_embedding_with_text` and a context pass in maintenance; re-probed, and all
three corpora now index.

**What remains is a model fact, not a wiring defect, and the probe separates the two.** With all three
corpora indexed, `giap-context` registered (confirmed in the log: 16 extensions including it) and the
`recall` tool offered, **gemma-4-E2B never calls it** — `recall_tool_seen=false` on every question. It
answers "I do not have any information in my memory or context" while the answer sits indexed one tool
call away. This is the same shape PAI-6 records for `delegate`: a 2B declines to call the tool, which
is a model-floor decision rather than something to patch. It is also the strongest argument yet for
G's passive prompt tier — but that remains a MEASUREMENT, and the probe is now the instrument for it.

One thing the probe caught in passing: the assistant's answers still leak harness text ("The goal is
not fully met"), which is the measured-bad prompt rule 6 recommended dropping, still live on `main`.

**Owed as of 2026-08-14, and all four are now closed.** (1) The re-embed path for stale-width vectors
is `search_stale_dimension`, in the port, `sqlite_memory.rs`, the redacting wrapper and a caller in
`memory_relevance.rs`. (2) `embed_query` splits the prefixes — `search_query: ` on the question,
`search_document: ` on stored text — at all three query sites. (3) `cargo test -p pond-inference` is
a CI job. (4) The Orin run is done; see below.

### 2026-08-17 — the deployed-binary run, and what it found was wrong with the pond rather than the code

**0a is fully closed.** With `embedding_provider = "gguf"` the deployed binary on the Orin reports
`GGUF embedding provider ready model_id="nomic-embed-text-v1.5" dims=768`, then
`GGUF embedding model loaded n_gpu_layers=0 max_ctx=2048`, and answers HTTP 200. The maintenance
pass then re-embedded the stale 384-dim vectors and restamped the index from `all-MiniLM-L6-v2` to
`nomic-embed-text-v1.5` — so the stale-width path is proven live, not just unit-tested.

**And the reason it had never run is not in this repository.** The pond's own settings said
`embedding_provider = fastembed` — the provider §1.1 records as unable to initialise on this
hardware. The GGUF work landed on 2026-08-13 and the device was never switched over, so §1.1's
headline defect ("semantic memory injection has never run on the hardware GIAP ships to") stayed
true for four more days while every phase above it was recorded as landed. The model bytes had been
sitting in `models/embedding/` since 2026-08-14. **Check what the device is configured to do before
believing what the code can do.**

**The coverage number was read wrong, including by me.** The index served ~20 rows against 276
memories, which reads as 7% coverage. It is not: 62 of those memories are archived and 209 merged,
so **five are live** — and all five were indexed. Coverage of live rows was complete. `CorpusHealth`
counts only rows that QUALIFY, which is the right denominator and the one that makes this legible;
the raw row count is not.

**What is actually empty, and why.** Three independent causes, none of them the index:

1. `context_items` is **zero**. No source has ever been connected, so PAI-8's corpus is empty by
   construction. That is the connector phases (P3–P8), not this document.
2. `sessions.profile_id` is NULL on all 651 sessions, so the summary corpus is refused. Traced to
   its actual cause: `Handshake::issue_pairing_code_for` has accepted a member since PAI-1 P9 and
   **no shipped caller ever passed one**, which the port's own doc states. Twelve devices, none
   attributed, so every turn falls through the paired-device rung. Fixed for a one-member household
   in `pairing_attribution::owner_for_new_code`, with `{"unattributed": true}` as the escape hatch;
   existing devices need re-pairing to pick up an owner.
3. Only five memories are live at all. A pond this small has little to retrieve regardless.

**The lesson worth carrying.** Every one of these was a complete mechanism with nothing driving it —
an embedder never switched on, a member parameter no caller passes, a corpus with no connected
source. This programme's recurring defect is not broken code; it is code that is finished, correct,
and unreached, which no test fails on and no log complains about.
