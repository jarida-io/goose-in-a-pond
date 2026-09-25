# Context & Reasoning Roadmap

State of the world (verified against code, 2026-07-27) and the phased plan for:
relevant context (Memories, Tools, Message History), letting the model reason
across as many turns as it needs, memory consolidation hardening, and — after
that — multimodality (image, audio, video).

All file:line references are to the live GooseAdapter path
(`crates/pond-adapters-goose/src/goose_agent.rs` unless noted). The
quarantined PondAgent loop (Q2-05) is out of scope.

---

## 1. Where we are today (verified)

### Memories

- **Injection is not semantic.** Per turn (when `agent_memory_inject`, default
  on, limit 5) GIAP merges `search_recent` (created_at DESC) with
  `search_by_content` — an OR'd SQL `LIKE %kw%` over every word ≥ 3 chars of
  the user message, stopwords included (`goose_agent.rs:1064-1104`). The
  fastembed embedding provider (all-MiniLM-L6-v2) is initialized at startup but
  is only reachable through the explicit `recall_memories`/`save_memory` MCP
  tools — never on the injection path.
- **Extraction memories are unembedded.** `MemoryFragment::from_extraction`
  stores `embedding: None` (`memory.rs:191-219`) and nothing backfills, so the
  vast majority of memories are invisible to vector search. Worse: once a few
  `save_memory` (embedded) rows exist, `search_similar` considers ONLY those
  and ignores every extraction memory.
- **Dedup is lexical substring vs the 20 most recent** rows, twice
  (`llm_memory_extractor.rs:188-195`, `memory_extraction.rs:89-97`).
  Paraphrases duplicate freely.
- **Budgeting**: merged fragments sorted importance-DESC, truncated to the
  profile cap, greedily packed under `memory_token_budget` (chars/4). On the
  Jetson tier (ctx ≤ 4096) that is 200 tokens / 3 fragments — the same
  high-importance identity memories dominate every turn.
- **Placement is right**: memories render inside
  `<system-context><memories>` prepended to the user message, keeping the
  system prefix token-stable for KV reuse. Keep this invariant.
- Decay/cleanup runs by default (adaptive half-life, archive < 0.15,
  prune < 0.05). `record_access` reinforcement fires on injection.

### Tools

> Superseded by Phase D below (landed). Kept as the measured baseline.

- **No relevance selection anywhere.** 14 `giap-*` extensions (57 tools when
  all enabled; AGENTS.md's "12" is stale) are registered at startup from
  settings toggles; every session loads all of them; `allowed_tools` per turn
  is the full cached union (`goose_agent.rs:1452-1547`); goose's
  `prepare_tools_and_prompt` sends `list_tools(None)` wholesale. The shim's
  allow-set is a veto (drops goose self-injected tools), never a narrower.
  Cost: ~100 tok/tool through the Gemma template ≈ 5.7K prompt tokens every
  turn on an 8K-class budget. (Measured after D1/D2 landed, with `giap-toolkit`
  added: 59 tools = 24,032 tools-JSON chars = 6,539 real prompt tokens.)
- `giap-schedule` alone is 12 of 57 tools.
- **ShimControls is one global slot per adapter** (`provider_shim.rs:66`) —
  fine while every session gets the same set; a data race the moment per-turn
  or per-session selection exists. Must be keyed before any selection work.
- **Tool results enter history verbatim** below goose's 200K file-spill
  threshold. GIAP's `TOOL_RESULT_MAX_CHARS = 1500` truncation only affects the
  trimmer's token ESTIMATE — the rebuild keeps structured ToolResponse
  messages whole, so a 50K tool result is re-prefilled every turn until its
  turn is dropped.

### Message history

- Model context comes from goose's `sessions.db` conversation, not
  `pond_system.db`. The GIAP→goose session mapping is **in-memory only**, and
  goose session ids are auto-generated: after a pond-server restart an
  existing chat resolves to a brand-new EMPTY goose session. The model loses
  the whole conversation even though pond_system.db has every message. Nothing
  replays/hydrates.
- `hybrid_compaction_enabled` defaults false, so out of the box the only
  defenses are goose's reactive LLM auto-compaction (threshold 0.8 — an
  on-device stall) and the 200K spill. With hybrid on, the deterministic
  trimmer drops whole oldest turns and splices the idle rolling summary.
- `GOOSE_CONTEXT_LIMIT` / `GOOSE_AUTO_COMPACT_THRESHOLD` are only (re)written
  when the provider:model key changes (`ensure_provider_current` early
  returns) — toggling hybrid compaction or context_window_override does
  nothing until a model switch or restart.
- Goose's tool-pair summarization (default ON) spawns background LLM calls to
  summarize old tool pairs — spending scarce on-device tok/s even in hybrid
  mode, double-owning tool-result pruning with the GIAP trimmer.
- Token estimation is chars/4 everywhere GIAP-side despite the engine knowing
  real counts (feedback multiplier exists but corrects one turn late).

### Turn budget (reasoning length)

- The live cap is `agent_max_turns` = **20 turns text / 8 voice** (a turn =
  one provider call). Goose's own default (1000) is unreachable because GIAP
  always passes `Some(effective_max_turns())`.
- On cap: the model's stream ends with MAX_TURNS_MESSAGE ("… Would you like
  me to continue?") — but nothing is wired to actually continue, and the
  message is not persisted in goose's own store (histories diverge).
- **The model never sees its budget**: goose's MOIM `<turn-context>` carries
  "N/M turns used" but the GIAP shim strips ALL turn-context blocks, so the
  model cannot pace itself or wrap up.
- Retries don't consume budget, but goal/grind nudges do.
- `/chat/stream` has an idle (silence) timeout `agent_timeout_secs` (300s
  default; resets on every event); `/agent/chat/stream` has none.
- Thinking: prompt-level `thinking_mode` renders a `<thinking>` section;
  engine-level `enable_thinking` is a registry default GIAP never sets, and
  the ThoughtFilter strips Harmony-style leakage. Prompt-off + engine-on is
  the current (inconsistent) combination when thinking is disabled.

### Memory consolidation (bonus track)

Real code, off by default (`memory_consolidation_enabled: false`):
three-stage adversarial pipeline (Proposer → Adversary → Judge, cancellable
between stages), inactivity trigger, SSE streaming UI, correction-safety
guards, audit table. Verified defects:

1. Fires ~15 min after every boot with zero activity (`last_user_activity`
   initialized to now; no startup guard, unlike the summary loop) — violates
   the "never on startup" constraint — and re-fires every ~15 min while idle.
2. Three settings are dead: `memory_consolidation_mode`,
   `_interval_hours`, `_batch_size` are persisted, UI-exposed, and never read.
   Trigger is hardcoded 15 min; mode is always adversarial; no batch cap.
3. No batch cap: ALL scoreable memories go into each of the three prompts —
   context blowup on a 3B/Jetson model as the store grows.
4. Apply-logic duplicated: `pond-core` `run_consolidation` (tested, handles
   Split/Recategorize) is production-unused; `main.rs:4182-4346` reimplements
   it inline. Drift risk.
5. Toggling the setting requires a restart (runner + loop built from a
   startup snapshot).
6. Dead code: single-pass `LlmMemoryConsolidator` (only an ignored live test
   calls it), orphaned `ports/adversarial_consolidator.rs` (references a
   nonexistent type, not in mod.rs).
7. `docs/architecture/memory_system.md` describes the old single-pass/24h
   design — stale.
8. Only chat/agent-chat/routine-run reset activity; voice-child and other API
   traffic do not cancel a running consolidation.

### Multimodality (scouted for phase F)

> Superseded by the Phase F section below, which records what was actually
> found on implementation. Two corrections in particular: the mmproj was NOT
> auto-downloaded for GIAP-registered models, and the MCP camera bridge could
> not be done through MCP alone.

- The engine's mtmd vision path WORKS (mmproj auto-download, MtmdBitmap
  tokenize/eval, vision-capable Gemma E2B/E4B registry entries, no-vision
  fallback). The REST API already accepts `ChatRequest.images`. **The single
  blocking gap**: `goose_agent.rs:1588` builds the user message with
  `with_text` only — `request.images` is dropped on the floor.
  `Message::user().with_image(data, mime)` exists and types line up.
  *(Correction from implementation: "mmproj auto-download" holds for goose's own
  featured-model flow, not for GIAP's registration path — see Phase F.)*
- Missing around it: desktop attachment UI + `images` in `ChatStreamRequest`;
  `/agent/chat/stream` hardcodes `images: vec![]`; images aren't persisted to
  history (a follow-up about an earlier image loses the pixels); vision turns
  forfeit KV prefix reuse (engine drops the retained session); `vision_capable`
  isn't surfaced to the UI; E4B+mmproj is at/over the 8GB Jetson budget (E2B
  is the realistic on-device vision target).
- Audio: dead-ends at goose's message types (`RawContent::Audio` →
  "[Audio content: not supported]") even though mtmd reports audio support —
  needs a fork-side content variant + extract path. Whisper ASR remains the
  transcription route meanwhile.
- Video: no path; realistic v1 is frame sampling → image path. Camera
  snapshots exist on disk (`camera_events.snapshot_path`) but vision MCP
  tools return text only.

---

## 2. The plan

Ordering principle: relevance first (biggest quality win per token), then
reasoning length, then history durability, then tool surface (KV-sensitive),
then consolidation hardening, then multimodality.

### Phase A — Relevant memories (semantic injection)

- A1 Embed at write: pass the embedding provider into the extraction
  pipeline so every new memory is embedded; startup backfill job for rows
  `WHERE embedding IS NULL` (batched, idle-friendly).
- A2 Semantic injection: embed the user message per turn (fastembed, CPU,
  ~ms) and call `search_similar`; merge recency + semantic (replace the
  naive LIKE keyword fetch; keep it only as a no-embedding fallback with a
  stopword filter).
- A3 Ranking: blend similarity, importance, and recency decay for the budget
  cut instead of importance-only, so topical memories can displace the
  standing identity block.
- A4 Semantic dedup at write: cosine against top-K similar (not substring vs
  20 recent).
- Invariants: memories stay in `<system-context>` in the user message (KV
  prefix stability); Jetson tier budgets unchanged until measured.

### Phase B — Reasoning length (turns) — LANDED (B5 deferred)

- B1 DONE `default_agent_max_turns` 20 → 50, and `0 = uncapped` (expressed to
  goose as `UNCAPPED_MAX_TURNS = 100_000`, safe to do arithmetic on unlike
  `u32::MAX`). Cancellation, idle timeout, and context-overflow abort remain the
  rails. Voice keeps its own cap: a non-zero `voice_max_turns` still binds even
  when the text budget is uncapped.
- B2 DONE, PER-REQUEST rather than per-turn. A `<turn-budget>` note goes into
  the user message's `<system-context>` (`turn_budget_note` in pond-core). The
  ≥ 50%-of-cap trigger from the original plan needs `turns_taken` mid-loop,
  which GooseAdapter cannot see — it builds the user message once, before
  `agent.reply`. A mid-loop injection would need a fork-side seam (see below).
- B3 DONE `AgentStreamEvent::TurnLimitReached { max_turns }`, detected by
  matching goose's private `MAX_TURNS_MESSAGE` verbatim (canary test
  `goose_cap_message_is_still_verbatim` reads the fork source so a reword
  fails). Surfaced as a `turn_limit_reached` SSE event from both stream routes;
  both desktop chat surfaces render a Continue action. The cap text is still
  emitted as Text so persistence and voice are unchanged.
- B4 DONE `enable_thinking` is passed as a ModelConfig request_param for
  `local`/`gguf` (the only provider that reads it), resolved from
  `thinking_mode` + voice + capabilities. A thinking-mode change re-stamps the
  config on the retained provider instead of rebuilding it.
- B5 NOT DONE — excluding goal/grind nudges from the turn count is inside
  `goose/crates/goose/src/agents/agent.rs`, i.e. a fork change. Deferred to a
  goose-side patch.

### Phase C — History durability & budgets — LANDED

- C1 DONE The pairing is persisted in `pond_system.db` (`engine_session_map`,
  migration 0032) behind two engine-neutral `SessionStorage` methods, and
  re-validated against goose on read (its store can be wiped independently). On
  a miss with existing pond history the new goose session is hydrated via
  pond-core's `plan_replay` (recent turns + rolling summary, budgeted by the
  same trimmer, trailing user message dropped because the handler persists it
  before the stream). Text-only: pond rows cannot rebuild a valid tool
  request/response pair.
- C2 DONE `GOOSE_CONTEXT_LIMIT` / `GOOSE_AUTO_COMPACT_THRESHOLD` (plus C3's
  knob) moved into `apply_goose_env_knobs`, called on the settings path every
  turn and `set_var`-ing only when the signature changes.
- C3 DONE `truncate_head_tail` (pond-core) is applied to the retained structured
  `ToolResponse` at rebuild — cloning the message and rewriting only text bodies,
  so ids, annotations, error flags and pairing are preserved — and to the
  trimmer's estimate, so estimate and reality agree.
  `GOOSE_TOOL_PAIR_SUMMARIZATION=false` for local/gguf + hybrid: the
  deterministic trimmer owns tool-result pruning on-device.
- C4 DONE `hybrid_compaction_enabled` now defaults to true.

### Phase D — Tool relevance (KV-aware) — LANDED (D3 deferred)

- D1 DONE Per-turn shim control state is keyed by GOOSE session id
  (`SessionControls` inside `ShimControls`, bounded LRU). The shim resolves the
  key from `goose::session_context::current_session_id()` — the `tokio::task_local`
  goose already wraps every provider call in (`reply_parts.rs`, the same one that
  stamps its `agent-session-id` header), so no fork change. A shim INSTANCE per
  session was rejected: `Agent` holds ONE `provider: Mutex<Option<Arc<dyn Provider>>>`,
  so per-session instances relocate the race into goose's provider slot instead of
  removing it. `system_prefix` and `extension_appendix` stay GLOBAL by design —
  the prefix IS the KV prefix and `override_system_prompt`/`last_prefix_hash` are
  agent-wide, so anything session-specific rides the user message's
  `<system-context>` instead.
- D2 DONE `tool_selection_mode` = `"all"` (default) | `"relevant"`. In
  `"relevant"`, a session's groups are chosen ONCE from its first message plus
  the injected memories: cosine (fastembed MiniLM, the Phase A embedder) against
  one natural-language description per EXTENSION (`pond-core`
  `mcp/domain/tool_group.rs`), core groups always in, everything at or above 0.28
  in, plus the top non-core scorer rescued below threshold. Sticky, cached
  in-process and persisted (`session_tool_groups`, migration 0033), so the tools
  JSON — and the KV prefix — does not churn between turns. Every failure path
  widens to ALL groups (no embedder, embed error, empty registry, unrecognised
  mode string). Narrowing is enforced at the SHIM, not by loading/unloading
  extensions: goose keeps offering all 27 tools and the veto drops the dormant
  ones, so a widen lands on the next provider call of the same reply loop.
  Core set: `giap-memory` (cross-cutting), `giap-system` (holds
  `get_current_time`), `giap-toolkit` (the hatch). `giap-draft` was the fourth
  until its group was deleted on 2026-09-10.
  Escape hatch: `giap-toolkit`'s `list_tool_groups` + `enable_tool_group`, plus a
  `<tool-groups>` listing of dormant groups in `<system-context>` so the model
  usually skips the discovery round trip. This is NOT a keyword classifier and
  never decides IF tools are used — it decides which schemas are in the prompt,
  for cost, and the model can reverse it itself.
  Measured (Mac, Gemma E2B Q4_K_M, same question, fresh sessions):
  59 tools / 24,032 tools-JSON chars / **6,539 prompt tokens** / 11.4s TTFT →
  17 tools / 6,671 chars / **2,386 prompt tokens** / 4.0s TTFT. A 64% prompt-token
  cut. Verified live: the weather tool still fires identically in `"relevant"`
  mode, and `enable_tool_group("giap-schedule")` followed by
  `giap-schedule__list_schedules` succeeds inside a SINGLE turn (payload observed
  growing 17 → 29 tools mid-turn).
- D3 DEFERRED, deliberately. Compressing `giap-schedule` (12 tools) into one
  action-enum dispatcher is a semantic change to the tool surface, and explicit
  single-purpose tools are more reliable than one overloaded enum on the 2-4B
  models GIAP actually runs on-device — a small model picks the right tool from a
  list far more consistently than it fills a discriminated-union argument. D2
  already removes `giap-schedule` from the prompt entirely for sessions that do
  not need it, which captures most of D3's saving without the reliability risk.
  Revisit only with a measured tool-call accuracy comparison on E2B/E4B.
- Known cost, accepted: with per-session tool sets, two sessions alternating on
  one model diverge in the KV prefix at the tools block rather than at the
  history, so alternating chats re-prefill more. Single-active-session use (the
  on-device norm) is unaffected, and each session's own turn-to-turn reuse is
  preserved, which is what the stickiness protects.
- Not done: the prompt template's textual `available_tools` listing (rendered
  only for HTTP providers — `native_tools_json` suppresses it for local/gguf) is
  still the full list. It lives inside the static prefix, so narrowing it
  per-session would thrash `last_prefix_hash`; it costs nothing on the on-device
  path this phase targets.

### Phase E — Consolidation hardening (bonus) — DONE

- E1 DONE. Scheduling policy extracted to pond-core
  (`user_data/services/consolidation_schedule.rs`) as a pure `should_run` gate:
  *at most one run per `memory_consolidation_interval_hours`, and only after
  `INACTIVITY_THRESHOLD_SECS` of quiet following real user activity in this
  process lifetime*. The startup guard compares `last_user_activity` against an
  `Instant` captured before the server binds (the in-process analogue of the
  summary loop's `started_at`), so a booted-but-untouched server never runs. The
  post-run reset of the activity clock is gone — the interval floor is what
  prevents a re-fire, and faking activity confused the summary loop that shares
  the clock.
- E2 DONE. All three settings wired; nothing removed, so the UI and
  `types.ts` are untouched and `every_settings_field_is_dispositioned` is
  unaffected. `mode` dispatches: `"single"` (the existing
  `LlmMemoryConsolidator`, one LLM call, and already the *default* the code was
  ignoring) vs `"adversarial"` (three calls). Single-pass synthesises
  `TrialExchange`es so the SSE modal renders unchanged, with rationales that say
  no review was run.
- E3 DONE. `select_batch` caps at `memory_consolidation_batch_size`,
  **oldest-first** (duplicates cluster in time, so a contiguous window is most
  likely to hold both halves of a pair) and reports `deferred` in the run log.
  Not done: the window does not rotate, so a store larger than the batch never
  reaches its newer half until the oldest are acted on — a persisted cursor
  needs a schema column and was deliberately left out.
- E4 DONE. The 165-line inline apply loop in `main.rs` is replaced by pond-core
  `apply_actions`, now shared by both modes. Comparing the two first: they were
  behaviourally identical arm for arm, so nothing needed porting *into*
  pond-core except the `consolidation_runs` audit insert. pond-core's `?` on
  `repo.add` was kept over main.rs's `let _ =` — a failed insert must not go on
  to supersede its sources.
- E5 DONE. Orphaned `ports/adversarial_consolidator.rs` deleted.
- E6 DONE. Runner and loop both built unconditionally; `enabled` is re-read per
  tick and in `start_consolidation`, so the toggle is live.
- E7 DONE, with a caveat. `AppState::note_user_activity` replaces four
  copy-pasted blocks and now also covers `/chat` and `/transcribe`. The voice
  child is a **separate OS process** and can never reach `AppState`, so activity
  became a two-source signal: the in-process clock *plus* the newest
  `sessions.updated_at` in `pond_system.db`, which the voice child bumps via
  `ChatService`. A watcher polls both during a run, so a voice turn cancels one
  in flight within ~500 ms.
- E8 DONE. `docs/architecture/memory_system.md` refreshed.

### Phase F — Multimodality — F1/F2/F3/F5 LANDED, F6 LANDED (Mac only) (F4 deferred)

**A second blocking gap the scouting missed.** `with_image` was necessary but
not sufficient. `GooseAdapter::register_gguf_model` hard-coded
`mmproj_path: None` for every GIAP-registered GGUF, and the engine's vision gate
is exactly that field (`has_vision = resolved_model.mmproj_path.is_some()`,
`goose-local-inference/src/llamacpp/mod.rs`). Goose's own
`enrich_with_featured_mmproj` could never help, because it matches
`featured_mmproj_spec(&self.id)` against the featured HF repo id
(`unsloth/gemma-4-E2B-it-GGUF`) while GIAP registers the bare stem
(`gemma-4-E2B-it`, `repo_id = "local/<stem>"`). Nothing in GIAP downloaded an
mmproj either. So before this phase, an image on the `local` provider could only
ever produce the engine's "[Image attached - image input is not supported...]".

- **F1 DONE.** `crates/pond-adapters-goose/src/vision_encoder.rs` maps a stem to
  its featured encoder, fetches it in the background to
  `<data_dir>/models/mmproj/<normalised-name>/` (NOT `models/gguf/`, which
  `resolve_gguf_filename` scans), and stamps `mmproj_path` /
  `mmproj_size_bytes` / `vision_capable` onto the registry entry.
  `resolve_model_path` runs on every `Provider::stream`, so the stamp takes
  effect on the next turn without a provider rebuild or a restart. The download
  is deliberately NOT blocking (the E2B encoder is 941 MB); a turn that needs
  bytes that have not landed gets a specific error saying so, rather than a
  silently rewritten prompt.
  Also: `attach_images` folds `request.images` onto the user message;
  `/agent/chat/stream` hand-parses `images` for parity; both chat surfaces got
  picker + paste + thumbnail + remove, with client-side downscale to 1024 px.
- **F1 gating**: no new endpoint. `GET /api/v1/models/capabilities` already
  returned `vision` and the desktop already read it — it was just untruthful,
  coming from a name regex that calls every `gemma-4*` vision-capable including
  `gemma-4-E1B-it`, which ships no encoder. For `local`/`gguf` it now comes from
  the registry. It reports DECLARED vision, not downloaded, so a user who has
  just picked a vision model is not told the model cannot see.
- **F1 limits**: `pond_core::models::domain::image_limits` — 4 images/turn,
  4 MiB each, 8 MiB total, MIME allowlist; size computed from base64 length
  WITHOUT decoding, so an oversized payload is a 413 before any decode buffer
  exists. Note Axum's `DefaultBodyLimit` is 2 MiB, *smaller than one legal
  image*: both chat routes carry an explicit `MAX_CHAT_BODY_BYTES` layer, sized
  so every domain rejection stays reachable with its actionable message.
- **F2 DONE — persisted, with a bounded replay.** `message_attachments`
  (migration 0034) indexes bytes stored under `<data_dir>/attachments/<sid>/`;
  bytes on disk rather than a SQLite BLOB because pond_system.db is read on
  every turn and every session listing. `add_message` writes them, best-effort
  (a full disk loses a picture, never the conversation).
  Replay is capped at `MAX_HISTORY_REPLAY_IMAGES = 1`, newest-first, with
  `[an image was attached here but is no longer available in this context]`
  for the rest. The cap is not timidity: ANY conversation containing an image
  makes that turn multimodal, and multimodal turns bypass KV retention — so
  replaying everything would make every later turn in the session pay a full
  prefill forever. `hydrate_goose_session` joins planned messages back to their
  stored attachments by message id (it pre-applies `plan_replay`'s own blank and
  trailing-user filters so the returned `index` is a valid subscript).
  Verified end to end: after a restart with a fresh goose session,
  `history_hydrate` logged `images_replayed=1` and the model answered a question
  about an image the engine session had never seen.
- **F3 DONE, via the shim — the MCP path alone cannot work here.** `rmcp 1.5.0`
  has `Content::image` and `CallToolResult` carries it fine, but two fork-side
  facts kill it on the on-device path: `multimodal.rs` matches only a TOP-LEVEL
  `MessageContent::Image` (a `ToolResponse` falls into its catch-all), and
  `strip_image_parts_from_messages` runs UNCONDITIONALLY, overwriting the
  `image_url` part that `formats/openai.rs` relocates. Both are submodule edits,
  out of scope this phase. So `GiapProviderShim::promote_tool_result_images`
  lifts tool-result images into a trailing user message — the one place the
  engine's extractor looks — gated to the `local`/`gguf` inner provider because
  the HTTP formats already relocate correctly and would otherwise double up.
  New tools: `look_at_camera_snapshot`, `look_at_camera_window` (both removed with the
  `giap-vision` group on 2026-09-10; see `pai/00-checklist.md`. The promotion path stays for
  user-added MCP tools that return pictures, and since F6 it checks the encoder is ready first).
- **F5 DONE as camera-event window sampling**, `look_at_camera_window`: up to 4
  evenly-spaced frames (`pick_evenly_spaced` always includes both ends, so three
  frames show a change rather than one moment three times). Deliberately NOT
  ffmpeg: `pond-adapters-vision` already decodes the stream and writes one JPEG
  per event, indexed by `camera_events.snapshot_path`. Those frames are decoded
  and on disk, so sampling them is strictly cheaper than re-spawning ffmpeg —
  and there is no recorded clip to sample anyway (the pipeline stores frames,
  not video). If clip recording ever lands, `FfmpegFrameSource::build_args` is
  the place to extend.
- **F4 still deferred (fork work).** What the patch involves, concretely:
  `RawContent::Audio` is flattened to the literal text
  `"[Audio content: not supported]"` in `From<Content> for MessageContent`
  (`goose-provider-types/src/conversation/message.rs`). The patch needs (a) a
  `MessageContent::Audio` variant plus a `with_audio` builder, (b) that `From`
  arm preserving it, (c) an `extract_audio_from_messages` beside
  `extract_images_from_messages` in `goose-local-inference/src/multimodal.rs`
  feeding mtmd's audio bitmaps (mtmd already reports audio support), (d) the
  HTTP format layers deciding to drop or relocate it, and (e) the same
  unconditional `strip_image_parts_from_messages` problem for audio parts.
  That is five touch points across two fork crates — a milestone, not a phase
  tail. Whisper ASR remains the audio route.
- **F6 LANDED 2026-09-24 (Mac only; the Orin run is owed) — picture support sets
  itself up.** F1's fetch is replaced, because on the Orin it had already failed
  in every way it could: the E2B encoder there was 636,790,074 of its 986,833,728
  bytes and counted as ready (the only check was "not empty"), and the Orin's
  active model, `gemma-4-E4B-it-qat-UD-Q4_K_XL`, never resolved to an encoder at
  all. What replaced it:
  - A pinned pond-core table (`vision_encoder::ENCODER_SPECS`), keyed by family
    AND qat-ness: qat and non-qat Gemma 4 encoders are different files with
    identical sizes, so identity is the HF LFS sha256, checked once and kept in a
    `.verified` sidecar.
  - The fetch goes through pond-hf-cache, pinned to a revision: resumable, gated
    per redirect hop, locked against a second process. It runs in the serve
    process only, and is refused cleanly by `network_mode`.
  - A bad file is set aside (renamed), never deleted, and fetched again.
  - Before any load, the provider build stamps every registry row naming the
    GGUF, or clears them.
  - Triggers: serve start (the active chat model), a finished model download,
    activation, and a chat-model change.
  - `GET /models/vision-status`, and the chat routes refuse a picture turn that
    is not ready with 409 before anything is saved. WebP is transcoded (the
    engine's `stb_image` cannot read it).
  - A picture the engine could not read is scrubbed from the session, so one
    failure cannot poison the conversation.
  - Both shells show the state in a status line and keep the draft and the photo
    when a turn is refused.

  On a budgeted device, a model declares vision only if its encoder fits without
  costing the window anything AND has been measured there.
  `device_budget::DEVICE_MEASURED_VISION` ships empty, so the Orin declares no
  picture support for now and no longer loads its broken E2B encoder at every E2B
  load. E4B there would drop from 16,384 to 2,048 tokens with the BF16 encoder,
  62% of which is an audio tower the engine cannot use. F1's "still downloading"
  wording, its "no new endpoint" gating and the burn-in note below are superseded
  by this entry.

**Measured (Mac M-series, gemma-4-E2B-it Q4_K_M, 60 tools, same question, fresh
session each, model already resident):**

| | prompt_tokens | ttft_ms | prefill_ms | prefill tok/s |
|---|---|---|---|---|
| text turn | 6,791 | 12,467 | 12,142 | 559 |
| image turn (256x256 PNG) | 7,079 | 14,985 | 14,913 | 475 |

+288 prompt tokens and +2.8 s prefill for one image. The image itself is 256
mtmd tokens (`image_tokens->nx = 256`); the engine logs `encoding image slice
882 ms` + `image decoded 50 ms`, so roughly a third of the delta is the vision
encoder and the rest is the larger prefill at a lower effective rate. The model
described a synthetic red square with a black circle correctly ("The background
color is red. The shape in the middle is a black circle."). Not yet measured on
the Jetson — E2B weights (~1.8 GB) plus a 941 MB encoder is a real bite out of
the 8 GB budget and needs its own burn-in before vision is recommended there.

**Field note worth keeping:** the first version of `look_at_camera_snapshot`
said "use this whenever the user asks what something LOOKS like". With an image
ATTACHED to the message, gemma-4-E2B called the camera tool instead of looking
at the picture it had already been given, got "No frame available", and answered
"I cannot see the image". Tool descriptions that overlap an intrinsic capability
need the boundary spelled out; both `look_at_*` descriptions now end with "not
for an image attached to the message".
