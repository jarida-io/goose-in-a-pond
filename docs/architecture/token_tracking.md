# Token Usage Tracking

Two systems in GIAP count tokens. They answer different questions and are allowed to disagree.

| | **Accounting** | **Budgeting** |
|---|---|---|
| Question | what did that turn cost? | how much fits in the next prompt? |
| When | after the stream drains | before a prompt is assembled |
| Source | the provider's own `Usage` events, aggregated into `TurnStats` | the `TokenCounter` port |
| Accuracy | real when the provider reports usage; chars/4 only when it does not | never exact (section 2.2) |
| Consumers | `sessions` totals, assistant rows, `GET /usage/summary`, `TurnMetrics`, the `turn_stats` SSE frame | `CompactionProfile`, `turn_trimmer`, `ContextGovernor` |

---

## 1. Accounting: what the turn cost

### 1.1 Flow (REST chat route)

```
  GooseAdapter::chat_stream
        |  aggregates every per-inference Usage event of the turn into TurnStats
        |  (a turn with tool round-trips has several inferences)
        v
  AgentStreamEvent::Done { usage: Some(UsageStats), stats: Option<TurnStats> }
        |
        +--> SSE `turn_stats` frame (turn_stats_frame in pond-api routes.rs)
        |
        +--> ChatService::persist_assistant_turn(.., Some((prompt, completion)), model)
        |       -> session_messages.prompt_tokens / completion_tokens on the assistant row
        |       -> SessionStorage::increment_usage -> sessions.total_* += N
        |
        +--> TurnMetrics -> pond_logs.db turn_metrics   (when settings.telemetry_enabled)
        |
        +--> SSE `done` frame: { usage: { prompt_tokens, completion_tokens } }
```

The voice path goes through `ChatService` directly: `record_turn_outcome` calls `increment_usage`
and records `TurnMetrics`, and `persist_assistant_response` writes the assistant row.

### 1.2 Real counts are the primary path

`TurnStats` (`crates/pond-core/src/shared/domain/turn_stats.rs`) is the per-turn aggregate:

| Field | Meaning |
|---|---|
| `prompt_tokens` | prompt size of the **final** inference: the turn's real context load |
| `prefilled_tokens` | prompt tokens actually decoded, summed over inferences (the prefill-rate basis) |
| `reused_prefix_tokens` | prompt tokens served from a retained KV cache; `None` for HTTP providers |
| `completion_tokens` | generated tokens, summed over inferences |
| `reasoning_tokens` | hidden reasoning tokens counted by GIAP (section 1.4); `None` = not counted |
| `context_used_tokens` / `context_limit_tokens` | final prompt size and the engine's allocated `n_ctx`; `context_pct()` derives the percentage |
| `ttft_ms`, `model_load_ms`, `prefill_ms`, `decode_ms`, `*_tok_per_sec` | timings; `finalize_rates()` derives the rates |
| `inference_count`, `reengagements`, `draft_accept_rate` | loop shape and speculative-decoding acceptance |

When the provider emitted `Usage` events (`saw_usage`), `GooseAdapter` reports `UsageStats` straight
from `TurnStats` and sets `context_used_tokens = prompt_tokens`.

### 1.3 The chars/4 fallback

Only for providers that emit no `Usage` events at all:

```
prompt_tokens     ~= user_message.len() / 4          (floored at 1)
completion_tokens ~= accumulated_output_chars / 4    (floored at 1)
```

In that case `stats` on the `Done` event is `None` (`saw_usage.then_some(turn_stats)`), so a
consumer that needs real numbers can tell. `UsageStats` itself carries no provenance flag, and the
`sessions` totals mix real and fallback counts without distinguishing them.

### 1.4 Reasoning tokens

No provider reports hidden reasoning, so `GooseAdapter::count_reasoning_tokens` counts thinking
blocks with the same `TokenCounter` the budget side uses (section 2.1). The count is reported
alongside `completion_tokens`, never subtracted from it.

| Where | Written by |
|---|---|
| `TurnStats.reasoning_tokens`, `UsageStats.reasoning_tokens` | `GooseAdapter::chat_stream` |
| `turn_metrics.reasoning_tokens` (logs migration `0008`) | both chat paths' telemetry |
| `session_messages.reasoning_tokens` (system migration `0039`) | the voice path only; `persist_assistant_turn`'s `(prompt, completion)` tuple cannot carry it, so REST rows stay `NULL` |

`NULL` means "not counted", never zero. `GooseAdapter::observed_reasoning_samples` reads the
non-`NULL` values among the newest 400 message rows (`recent_reasoning_samples`) to size the output
reserve (section 2.4).

### 1.5 Persistence

System database (`crates/pond-infra/migrations/system/`):

| Migration | Columns |
|---|---|
| `0016_session_usage.sql` | `sessions.total_prompt_tokens`, `total_completion_tokens` (NOT NULL DEFAULT 0), `model_name` |
| `0029_message_token_counts.sql` | `session_messages.prompt_tokens`, `completion_tokens` (assistant rows; user and tool rows stay `NULL`) |
| `0039_reasoning_tokens.sql` | `session_messages.reasoning_tokens` (nullable, no default) |

`SqliteSessionStorage::increment_usage` adds atomically (`persist_assistant_turn` skips the call
when both counts are zero):

```sql
UPDATE sessions SET
    total_prompt_tokens     = total_prompt_tokens + ?,
    total_completion_tokens = total_completion_tokens + ?,
    model_name = COALESCE(?, model_name),
    updated_at = datetime('now')
WHERE id = ?
```

Per-turn telemetry (`TurnMetrics`, `crates/pond-core/src/security/domain/turn_metrics.rs`) lives in
`pond_logs.db` (`turn_metrics`, logs migrations `0005`, `0007`, `0008`, `0010`) and is read through
`GET /api/v1/telemetry/turns` and `GET /api/v1/telemetry/summary`. Its `context_utilization_pct` is
`context_used_tokens` over the window `ContextGovernor` resolves for the turn (section 3).

---

## 2. Budgeting: how much fits

### 2.1 The `TokenCounter` port

`crates/pond-core/src/models/ports/token_counter.rs`:

```rust
pub trait TokenCounter: Send + Sync {
    fn count(&self, text: &str) -> usize;   // excludes the per-message envelope
    fn is_exact(&self) -> bool;             // true ONLY for the active model's own vocabulary
    fn name(&self) -> &'static str;         // label for logs and telemetry
}
```

It is called per message, so implementations must be cheap, never block and never call a model.
Injecting it keeps `pond-core` free of a tokenizer dependency.

| Implementation | Where | `name()` | `is_exact()` |
|---|---|---|---|
| `HeuristicTokenCounter` | `pond-core`, `models/services/context/token_counting.rs` | `chars/4` | `false` |
| `TiktokenCounter` | `pond-adapters-goose/src/token_counter.rs` | `tiktoken-o200k` | `false` |

`HeuristicTokenCounter` is `text.len() / 4`. `len()` is **bytes**, so multi-byte scripts are
over-counted, which is the safe direction (a smaller budget, not an overflowed window); the test
`multibyte_text_is_over_counted_which_is_the_safe_direction` pins it.
`PER_MESSAGE_TOKEN_OVERHEAD = 4` lives beside it and is added by the trimmer, not the counter.

`TiktokenCounter` wraps `goose::token_counter::TokenCounter`: `tiktoken_rs::o200k_base` with a
blake3-keyed LRU cache, embedded in the crate so it works offline. `GooseAdapter::token_counter_cell`
builds it once; on failure it logs `token counter unavailable, falling back to chars/4` and every
caller gets `HeuristicTokenCounter`.

### 2.2 No counter is exact

A counter is exact only with the active model's own vocabulary. tiktoken over a Gemma GGUF is a
real BPE over the real bytes and still inexact; it beats chars/4 mainly on JSON tool results and
non-Latin text. The active model's tokenizer sits inside `goose-local-inference`'s private
`llamacpp` module, and `pond-inference`'s tokenizer belongs to the quarantined `pond-agent` path with
its own model instance, so neither is wired in.

### 2.3 Who trims what

Goose owns the live conversation and compacts it itself. GIAP supplies the window through
`GOOSE_CONTEXT_LIMIT` (section 3.2) and leaves `GOOSE_AUTO_COMPACT_THRESHOLD` and
`GOOSE_TOOL_PAIR_SUMMARIZATION` unset, so Goose's defaults apply (`goose_env_knobs` in
`goose_agent.rs`).

GIAP's own trimmer, `turn_trimmer::trim_history`, runs through `plan_replay` when
`GooseAdapter::hydrate_goose_session` replays stored history into a fresh Goose session (for
example after Goose's `sessions.db` was wiped). It strips stale `<system-context>` blocks,
head+tail-truncates tool results to `TOOL_RESULT_MAX_BYTES` (1,500), splices the rolling summary,
optionally re-cuts tool results older than the verbatim horizon, and drops the oldest complete
turns until the estimate fits. Images are capped separately (`image_history`): a window keeps
one history image for each 8,192 tokens of it, eight at most, and when a conversation passes that
the oldest lose their pixels down to half of it in one rewrite.

The mistral.rs agent and the quarantined `pond-agent` budget history in characters through
`available_history_chars`.

### 2.4 `CompactionProfile`

`crates/pond-core/src/models/services/context/context_budget.rs`. `from_context_window` interpolates
piecewise-linearly over six anchors (4,096 / 8,192 / 12,288 / 32,768 / 65,536 / 128,000 tokens) and is
flat outside them; each anchor sets the compaction threshold, memory and system-prompt budgets, the
history budget and the output reserve.

`GooseAdapter::turn_profile` builds the live profile:

1. `CompactionProfile::for_windows(window, ContextGovernor::prompt_window(provider, window))`:
   preamble budgets from the clamped prompt window, history from the full window plus what the
   clamp frees.
2. `.with_history_reserved(fraction)`: holds back the share of history the session's live child
   agents hold (the orchestrator's `DeviceLedger`, keyed by GIAP session id).
3. `observed_output_reserve(samples, anchor, window)`: once at least 20 measured reasoning samples
   exist, the reserve becomes `2 * p95`, rounded up to 256, capped at 25% of the window, and never
   below the anchor.

`usable_prompt_tokens()` is the window minus the output reserve; `usable_history_tokens()` further
subtracts the system-prompt and memory budgets.

### 2.5 The overshoot correction

`turn_trimmer::effective_history_budget(profile, last_real_prompt_tokens)` shrinks the history
budget by however much a previous turn's **real** prompt overshot `usable_prompt_tokens()`,
floored at `MIN_HISTORY_TOKENS = 64`:

```rust
tokens = tokens.saturating_sub(real - usable).max(MIN_HISTORY_TOKENS);
```

It bounds the counter's error by measurement. `plan_replay` passes `None` (a replay has no previous
turn in the new engine session). `GooseAdapter` records each turn's real `prompt_tokens` per GIAP
session (`last_prompt_tokens_handle`), only when `saw_usage`, but no budget path reads that map.

---

## 3. The window budgets are measured against

### 3.1 `ContextGovernor`

`crates/pond-core/src/models/services/context/context_governor.rs` is the single answer to "how
big is this model's window". It is stateless: `resolve(&ContextInputs) -> WindowResolution { tokens,
source }`.

| Rung | `WindowSource` | Input | `is_exact()` |
|---|---|---|---|
| 1 | `EngineReported` | `EngineWindow { tokens, model }` from `TurnStats.context_limit_tokens`; ignored if the model differs | yes |
| 2 | `Registry` | pinned `context_size` in Goose's local model registry (`local` / `gguf` only) | yes |
| 3 | `CatalogRecord` | `ModelRecord.context_length`; clamped to 32,768 for on-device providers, and a lower override wins (reported as `Override`) | no |
| 4 | `Override` | `Settings.context_window_override` (0 = unset) | no |
| 5 | `Heuristic` | caller-supplied `capability_window`, else 32,768 for `local` / `gguf`, else `ModelCapabilities::from_model_name` | no |

Allocations (rungs 1-2) outrank the user override because budgeting past the allocation only makes
the engine truncate. `WindowSource::label()` returns `engine` / `registry` / `catalog` / `override` /
`heuristic`; no HTTP response or UI reads it.

Callers:

| Caller | Rungs it supplies |
|---|---|
| `GooseAdapter::resolve_window` (sets `GOOSE_CONTEXT_LIMIT`, feeds `turn_profile`) | 2, 3 (looked up only without a pin), 4, 5 without `capability_window` |
| REST chat telemetry in `pond-api` `routes.rs` | 1, 3, 4, 5 with `capability_window` from `agent.capabilities()` |
| mistral.rs agent, `pond-server` mistral.rs wiring, `pond-agent` | 4, 5 |

`ContextGovernor::prompt_window(provider, resolved)` clamps the **preamble** side to 8,192 for
`local` and `gguf` providers, which re-prefill it every turn; history budgets get the full window.

### 3.2 `GOOSE_CONTEXT_LIMIT`

`GooseAdapter::apply_goose_env_knobs` runs every turn and exports the resolved window as
`GOOSE_CONTEXT_LIMIT` (via `goose_env_knobs`) whenever the knob signature changes. Goose reads it:
its config's context limit (which its compaction plans against), Ollama's `options.num_ctx`, and the
in-process llama.cpp and LiteRT engines' context size when no registry pin applies. GIAP code never
reads it back; two source-scanning tests guard that:
`this_module_does_not_read_the_environment` (`context_governor.rs`) and
`no_budget_path_reads_the_context_limit_from_the_environment` (`goose_agent.rs`).

---

## 4. API

### `GET /api/v1/usage/summary`

Aggregates every session:

```json
{
  "total_prompt_tokens": 12500,
  "total_completion_tokens": 8200,
  "total_reasoning_tokens": 3100,
  "counted_reasoning_turns": 42,
  "total_tokens": 20700,
  "session_count": 15,
  "cloud_input_price_per_million": 2.5,
  "cloud_output_price_per_million": 10.0
}
```

`total_tokens` is prompt plus completion; reasoning is reported separately and not priced.
`counted_reasoning_turns` tells "no thinking" apart from "not counted". The reasoning sum walks every
session's messages. Prices come from settings, defaulting to 2.50 and 10.00.

### `GET /api/v1/sessions`

Each entry carries `id`, `title` (stored, else derived from the first user message), `preview`,
`message_count`, `total_prompt_tokens`, `total_completion_tokens`, `model_name`, `created_at`,
`updated_at`.

---

## 5. Cost-savings settings

| Setting | Default | Where set |
|---|---|---|
| `cloud_input_price_per_million` | 2.50 | settings catalogue (`pond-desktop/src/settings/catalogue.ts`) |
| `cloud_output_price_per_million` | 10.00 | same |

The savings figure is `prompt / 1e6 * input_price + completion / 1e6 * output_price`, computed in
`pond-desktop/src/components/UsageStatsCard.tsx`. No view mounts that component and
`PondApiClient.getUsageSummary` has no caller, so the desktop does not show the figure. The per-turn
surface the desktop does show is `TurnStatsFooter` (`ctx used/limit`), fed by the `turn_stats` frame.

---

## Related documents

- [Model Capabilities](./model_capabilities.md): where `context_window_tokens` enters the governor.
- [PAI-3 Context governor](./pai/03-context-governor.md): the design and its phases.
- [Inference Optimization](../developer/inference_optimization.md): platform-specific inference settings.
