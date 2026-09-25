# Model Capabilities System

Runtime capability discovery for LLM providers. Each adapter reports what the active model
supports, and services adapt behaviour accordingly -- without any model-specific code in
`pond-core`.

Verified against code 2026-08-06. One thing to read before the rest: since PAI-3 P1,
`context_window_tokens` is **no longer the number budgets derive from**. It is the last rung of
`ContextGovernor`'s precedence, consulted only when nothing better is known. Section "What
capabilities drive" says what changed and what it broke.

---

## ModelCapabilities Struct

**File:** `crates/pond-core/src/models/domain/model_capabilities.rs`

```rust
pub struct ModelCapabilities {
    pub thinking: bool,              // Chain-of-thought reasoning (Gemma 4, Qwen3, DeepSeek-R1)
    pub vision: bool,                // Accepts image input
    pub audio_input: bool,           // Accepts raw audio (Gemma 4 E2B/E4B)
    pub context_window_tokens: u32,  // Declared context window (default: 4096)
    pub structured_output: bool,     // GBNF grammar / JSON mode (GGUF models)
    pub tool_calling: bool,          // Native tool calling (e.g. Gemma 4 `<|tool_call>`)
}
```

Six fields. The struct derives `Serialize`, and `GET /api/v1/models/capabilities` serialises the
whole thing, so **every field added here appears on the wire immediately** -- there is no DTO to
forget to update, and no filter. Adding a field is an API change.

When `tool_calling` is true, tool definitions are passed through the chat template; when false,
tools are described in the system prompt text instead.

All fields default to the most conservative value (`false` / `4096`), so an unknown model works
safely.

---

## Detection: `from_model_name()`

A heuristic over the lowercased model identifier. Each axis matches independently, so a name can
pick up some flags and not others.

| Pattern matched | thinking | vision | audio | context | structured | tool_calling |
|---|---|---|---|---|---|---|
| `gemma-4` / `gemma4` / `gemma_4` | yes | see below | E2B/E4B only | 128,000 | via quant suffix | yes |
| `qwen3` | yes | see below | no | 32,768 | via quant suffix | yes |
| `qwq` | yes | no | no | **4,096** | via quant suffix | no |
| `deepseek-r1` / `deepseek_r1` | yes | no | no | 4,096 | via quant suffix | no |
| `qwen` (any other) | no | see below | no | 32,768 | via quant suffix | no |
| `llama-3` / `llama3` | no | see below | no | 8,192 | via quant suffix | no |
| `mistral` | no | no | no | 32,768 | via quant suffix | yes |
| anything else | no | see below | no | 4,096 | via quant suffix | no |

Two rows that look like typos and are not:

- **`qwq` gets 4,096, not 32K.** The context arm matches the substring `qwen`, and `qwq` does not
  contain it. `qwq` is thinking-capable with a conservative window.
- **`gemma-4` is 128,000 for every size.** The code branches on E2B/E4B and returns the same value
  in both arms; the larger family members are conservatively under-declared.

`structured_output` is set by the **quantisation suffix**, not the family: `.gguf`, `q4_k`, `q5_k`,
`q6_k` or `q8_0` anywhere in the name. So `gemma-4-E2B-it` is `false` and
`gemma-4-E2B-it-Q4_K_M.gguf` is `true` for the same weights.

Note that the family spellings are deliberately literal (`gemma-4`, `gemma4`, `gemma_4`) on every
axis except vision. Widening them would silently change thinking, tool-calling and window
behaviour -- and with them the prompt tier -- for models beyond the one bug a wider list was needed
for.

### Vision is a separate function: `name_implies_vision()`

Vision is the only flag that can put a claim about the model's own senses into its prompt ("you can
see images"), so a false positive makes a text-only model answer an image question by inventing an
image, while a false negative merely withholds the `<vision>` prompt section. The rule set is
therefore biased hard towards `false` and lives in its own function:

1. **`GEMMA4_NAME_FRAGMENTS`** -- `gemma-4`, `gemma4`, `gemma_4`, `gemma-3n`, `gemma3n`, `gemma_3n`
   (the `3n` spellings are the same weights as served by Ollama and Hugging Face). Vision **unless
   the name contains `e1b`**: E1B is the one featured Gemma 4 with `mmproj: None`, and without the
   exclusion an HTTP provider serving it is told it can see.
2. **`VISION_NAME_FRAGMENTS`** -- distinctive family substrings: `llava` (covers `bakllava`),
   `moondream`, `pixtral`, `minicpm-v`, `minicpm_v`, `minicpm-o`, `internvl`, `cogvlm`, `smolvlm`,
   `idefics`, `multimodal`, and `qwen2.5vl` (Ollama publishes it unseparated, so the segment rule
   below cannot see it).
3. **`VISION_NAME_SEGMENTS`** -- `vision`, `vl`, `vlm` as a whole segment after splitting on any
   non-alphanumeric character. This is a *marker* rule, not a family identification: it is what
   covers `llama3.2-vision`, `qwen2.5-vl` and `qwen3-vl` without an entry each. Segment matching
   rather than `contains` is what keeps `vlad-tuned-7b` and `nvlink-test-model` out.

Cloud models are deliberately not listed. They are vision-capable, but they do not exhibit the
failure this signal exists to fix (a 4B model insisting it is "a text-based assistant"), and every
entry is a claim this file has to keep true.

---

## Trait Integration

### LlmProvider

**File:** `crates/pond-core/src/models/ports/provider.rs`

```rust
pub trait LlmProvider: Send + Sync {
    fn capabilities(&self) -> ModelCapabilities { ModelCapabilities::default() }
    // ... complete(), stream_complete(), model_name()
}
```

### Agent

**File:** `crates/pond-core/src/models/ports/agent.rs`

```rust
pub trait Agent: Send + Sync {
    fn capabilities(&self) -> ModelCapabilities { ModelCapabilities::default() }
    // ... chat(), chat_stream()
}
```

Defaults are conservative. Adapters override.

---

## Adapter Implementations

| Adapter | How capabilities are resolved |
|---|---|
| `GooseAdapter` (the live engine) | `from_model_name()` inside the provider-**swap** branch of `ensure_provider_current()`, then `vision` is overwritten by `model_supports_vision()`. Cached in `Mutex<ModelCapabilities>`. |
| `OllamaProvider` | `from_model_name()` on the configured model string. |
| `LocalInferenceLlmAdapter` | `from_model_name()` on `inner.model_name()`. |
| `GooseProviderAdapter` | `from_model_name()` on `model_config.model_name`. |

Two `GooseAdapter` behaviours matter more than the table suggests.

**`vision` does not come from the name alone on the in-process engine.**
`GooseAdapter::model_supports_vision` asks pond-core's device-aware declaration
(`device_budget::vision_declaration`): does the model have a pinned encoder
(`vision_encoder::encoder_for`, keyed by family AND qat-ness, because qat and non-qat Gemma 4
encoders are different files of identical size), and, on a budgeted device such as the Orin, does
that encoder fit without costing the window anything and has it been measured there
(`DEVICE_MEASURED_VISION`, empty as of 2026-09-24, so the Orin declares no vision). HTTP providers
fall back to `name_implies_vision`; `mesh` and `mistralrs` declare none, because the mesh wire is
text-only and a mistral.rs server has never been checked with a picture, and both would drop a
photo silently while the prompt said the model could see. It reports what is **declared**, not what
is downloaded: the encoder is ~941 MB and lands in the background, and a flag that flipped
mid-session would move the `<vision>` prompt section, which sits inside the KV-cached static
prefix. Whether the bytes are there, verified and ready is a separate question with its own answer,
`GET /api/v1/models/vision-status` (below), which never feeds the prompt.

**The cache is stale on turn one, and that stale read once cost 3.7 s per session.** The cache is
refreshed only inside the swap branch, which runs *later* in the same turn that builds the prompt,
so on the first turn of a process it still holds `ModelCapabilities::default()` with
`thinking: false`. Turn 1 rendered a prompt without the thinking section and turn 2 rendered one
with it -- 78 characters appearing at the top of the static prefix, moving `prefix_hash`, forfeiting
the engine's KV prompt-session prefix and paying one full re-prefill on every session's second
turn. `thinking_section_applies` therefore calls `from_model_name` directly rather than reading the
cache. Any new consumer inside the prompt-building path should do the same.

**`capabilities()` is not the cache verbatim.** In voice mode it returns `thinking`, `vision` and
`audio_input` forced to `false`: reasoning tokens waste TTS time and can leak as spoken text, and
there is no attachment path. `vision_section_applies` reads the same instance-level voice flag, so
the prompt cannot assert a capability the accessor simultaneously denies.

---

## What capabilities drive

### Context window (`context_window_tokens`) -- read this before using it

`context_window_tokens` is a **declared** window derived from a model name. Since PAI-3 P1 it is
the *last* rung of `ContextGovernor`'s five-rung precedence, supplied as `ContextInputs.capability_window`:

```
EngineReported  >  Registry  >  CatalogRecord  >  Override  >  Heuristic(capability_window, else from_model_name)
```

Consequences worth stating plainly, because the previous version of this document asserted the
opposite of two of them:

- **The override does not cap it.** `context_window_override` is rung 4. It loses outright to an
  engine-reported `n_ctx` and to a pinned registry `context_size`, because those are allocations
  and an override is a preference. It wins over the capability number.
- **`trim_to_budget_for_model()` has no production caller.** It still exists in `context_budget.rs`
  and still implements the old "reserve 20%, cap by override" rule, but the only call sites in the
  tree are its own unit tests. The live trim path is `turn_trimmer::trim_history` driven by
  `CompactionProfile::from_context_window(window.tokens)`, where `window` came from the governor.
  Do not cite `trim_to_budget_for_model` as evidence of what capabilities drive.
- **Its `is_exact()` is always false.** `WindowSource::is_exact()` is true only for
  `EngineReported` and `Registry`. A capability-derived window resolves at the `Heuristic` rung
  even when it is a better answer than the name would give, because it is still a declaration
  rather than an allocation.
- **The preamble does not scale with it.** `ContextGovernor::prompt_window` clamps the prompt-side
  budget to 8192 for `local` and `gguf` providers. A larger window buys history, not a more verbose
  system prompt.

Where the number still matters: it is the fallback the API telemetry path and `pond-agent` supply,
and it is what the Models UI renders as the `Nk ctx` badge.

`CompactionProfile::from_context_window` still buckets into four hardcoded tiers (`<= 4096`,
`<= 12288`, `<= 65536`, above), so a 24K model and a 64K model currently get identical budgets.
Replacing that with a continuous function is PAI-3 P4.

### Thinking (`thinking: true`)

- `PromptState.thinking_enabled` gates the `<thinking>` section of the system prompt template
  (`{%- if thinking_enabled %}` in `prompts.rs`).
- `GooseAdapter::thinking_section_applies` resolves it: `"on"` and `"off"` short-circuit, and
  `"auto"` asks `from_model_name(model).thinking` -- the name, not the cache, for the KV-prefix
  reason above.
- Always `false` in voice mode, for both the prompt section and the engine's `enable_thinking`
  request parameter, so the two cannot disagree.
- `ThoughtFilter` captures thinking blocks as SSE events when `settings.show_thinking` is on and
  the request is not a voice turn.

### Vision (`vision: true`)

- `ChatMessage.images: Vec<ImageAttachment>` carries base64-encoded images.
- `GooseAdapter` attaches images via `Message::with_image()` when vision is true.
- The `<vision>` system-prompt section is appended to the **template**, before Tera runs, so it
  lands inside `static_prefix` and `prefix_hash` covers it.
- Readiness is `GET /api/v1/models/vision-status`: `{model, state, size_bytes, message}`, where
  `state` is pond-core's `EncoderState` tagged by `kind` (`unknown`, `not_declared`,
  `not_on_this_device`, `absent`, `verifying`, `downloading`, `ready`, `failed`, `blocked`) and
  `message` is the household sentence for it. Both chat shells poll it (`useVisionStatus`) and gate
  sending pictures on it, and the chat routes refuse a picture turn that is not ready with 409
  `vision_not_ready` / `vision_unsupported` before anything is saved.
- The paperclip is never disabled for vision reasons; it is muted, and a tap or a paste shows the
  reason. `capabilities.vision` is only the fallback when vision-status cannot be read.

### Tool calling (`tool_calling: true`)

Tool definitions go through the chat template rather than being described in system prompt text.

---

## REST API

### `GET /api/v1/models/capabilities`

`get_model_capabilities` serialises `state.agent.capabilities()` whole, so the response carries
all six fields:

```json
{
  "thinking": true,
  "vision": true,
  "audio_input": false,
  "context_window_tokens": 128000,
  "structured_output": true,
  "tool_calling": true
}
```

There is no `window_source` field. `WindowSource` exists and has a `label()`, but nothing surfaces
it over HTTP or in the UI yet, so the Models page still cannot answer "why does this model think it
has 4K?" -- the question PAI-3 introduced the enum for.

---

## Frontend

- **Models page (`Models.tsx`)** -- per-model capability badges from `inferCapabilities`, a
  **frontend copy** of the detection heuristic (see the drift warning below), plus an
  `ActiveRolesBanner` that renders Thinking / Vision / Audio / Structured / `Nk ctx` badges from the
  real `GET /models/capabilities` response. The banner has no "Model features" label; the badges
  sit inline under the role chips. The two badge lists also use different context thresholds --
  the per-model list shows the ctx badge above 8192, the banner above 4096.
- **Chat (`Chat.tsx`, `ChatHub.tsx`)** -- sending pictures gated on `GET /models/vision-status`
  (falling back to `caps.vision` when it cannot be read), with an `ImageSupportStatus` line for the
  states in between. Model rows carry a server-computed `reads_images` rather than a name rule.
- **Settings (`Settings.tsx`)** -- Thinking mode selector (`auto` / `on` / `off`).

### `inferCapabilities` has drifted from `from_model_name`, in both directions

`Models.tsx` re-implements the heuristic in TypeScript for badges on models that are not the active
one (there is no per-model capabilities endpoint). Its comment claims it mirrors the backend. It no
longer does:

- It has **no E1B exclusion**, so `gemma-4-E1B-it` gets a Vision badge the backend deliberately
  refuses.
- It does **not** know the `gemma-3n` / `gemma3n` spellings, so those models get no badges at all.
- It has no `vision` / `vl` / `vlm` segment rule, so `llama3.2-vision` and `qwen2.5-vl` get no
  Vision badge.
- It computes neither `structured_output` nor `tool_calling`.

Only badges are affected -- nothing branches on it -- but it is a claim shown to the user, and a
second copy of a security-adjacent-shaped rule that nothing keeps in step. Either drive the badges
from a backend response or delete the duplicate; do not patch it and leave the divergence
unrecorded.

---

## Related Documents

- [Token Usage Tracking](./token_tracking.md) -- the counting side, and where the governor's window
  fits into it.
- [PAI-3 Context governor](./pai/03-context-governor.md) -- the precedence, its invariants, and the
  outstanding phases.
- [Agent Pipeline](./agent_pipeline.md) -- how capabilities affect the processing pipeline.
- [Components](./components.md) -- port trait definitions.
- [Inference Optimization](../developer/inference_optimization.md) -- platform-specific settings.
