# Model Capabilities

What the active model can do, as reported by the agent and its providers. `pond-core` carries the
type and a name heuristic; adapters refine it from better evidence where they have it.

---

## `ModelCapabilities`

`crates/pond-core/src/models/domain/model_capabilities.rs`

```rust
pub struct ModelCapabilities {
    pub thinking: bool,              // emits reasoning (Gemma 4 channels, <think> blocks)
    pub vision: bool,                // accepts image input
    pub audio_input: bool,           // accepts raw audio
    pub context_window_tokens: u32,  // declared window; default 4096
    pub structured_output: bool,     // GBNF grammar / JSON mode
    pub tool_calling: bool,          // tools via the chat template, else as system-prompt text
}
```

`Default` is the conservative value on every axis (`false` / `4096`). The struct derives `Serialize`
and `GET /api/v1/models/capabilities` returns it whole, so a new field is an API change.

---

## Name heuristic: `from_model_name()`

Substring matches over the lowercased model name; each axis is independent.

| Pattern | thinking | vision | audio_input | context | tool_calling |
|---|---|---|---|---|---|
| `gemma-4` / `gemma4` / `gemma_4` | yes | `name_implies_vision` | E2B / E4B only | 128,000 | yes |
| `qwen3` | yes | `name_implies_vision` | no | 32,768 | yes |
| `qwq` | yes | `name_implies_vision` | no | 4,096 | no |
| `deepseek-r1` / `deepseek_r1` | yes | `name_implies_vision` | no | 4,096 | no |
| other `qwen` | no | `name_implies_vision` | no | 32,768 | no |
| `llama-3` / `llama3` | no | `name_implies_vision` | no | 8,192 | no |
| `mistral` | no | `name_implies_vision` | no | 32,768 | yes |
| anything else | no | `name_implies_vision` | no | 4,096 | no |

- `structured_output` comes from the quantisation suffix, not the family: `.gguf`, `q4_k`, `q5_k`,
  `q6_k` or `q8_0` anywhere in the name.
- `qwq` gets 4,096 because the context arm matches the substring `qwen`, which `qwq` lacks.
- Gemma 4 gets 128,000 at every size.
- The Gemma 4 spellings stay literal on every axis except vision, so widening them cannot move the
  thinking, tool-calling or window behaviour of other models.

### `name_implies_vision()`

Vision is the one flag that puts a claim about the model's senses into its prompt, so the rule is
biased towards `false`:

1. `GEMMA4_NAME_FRAGMENTS` (`gemma-4`, `gemma4`, `gemma_4`, `gemma-3n`, `gemma3n`, `gemma_3n`): vision
   unless the name contains `e1b`, the one Gemma 4 without an mmproj.
2. `VISION_NAME_FRAGMENTS` (substring): `llava` (covers `bakllava`), `moondream`, `pixtral`,
   `minicpm-v`, `minicpm_v`, `minicpm-o`, `internvl`, `cogvlm`, `smolvlm`, `idefics`, `multimodal`,
   `qwen2.5vl`.
3. `VISION_NAME_SEGMENTS` (whole segment after splitting on non-alphanumerics): `vision`, `vl`,
   `vlm`. This covers `llama3.2-vision`, `qwen2.5-vl`, `qwen3-vl` and keeps `vlad-tuned-7b` out.

Cloud models are not listed.

---

## Ports

| Trait | File | Capability methods |
|---|---|---|
| `LlmProvider` | `crates/pond-core/src/models/ports/provider.rs` | `capabilities()`, default `ModelCapabilities::default()` |
| `InferenceProvider` | `crates/pond-core/src/models/ports/inference.rs` | `capabilities()` (same default); feeds only the quarantined `pond-agent` loop |
| `Agent` | `crates/pond-core/src/models/ports/agent.rs` | `capabilities()` (same default); `vision_state(provider, model) -> Option<EncoderState>`, default `None` (unknown); `prepare_model`, `forget_model_file` for picture add-ons |

---

## Implementations

| Implementation | Crate | How capabilities are resolved |
|---|---|---|
| `GooseAdapter` (`Agent`, the live engine) | `pond-adapters-goose` | cached; see below |
| `MistralRsProvider` / its agent | `pond-adapters-mistralrs` | name heuristic, then `tool_calling = true`, `context_window_tokens` from the provider, `vision = false`; `vision_state` is `NotDeclared` |
| `OllamaProvider` | `pond-adapters-ollama` | `from_model_name(model)` |
| `LocalInferenceLlmAdapter` | `pond-adapters-local-inference` | `model_traits::capabilities("local", model, data_dir)`, the same as `GooseAdapter` |
| `GooseProviderAdapter` | `pond-adapters-goose` | `from_model_name(model_config.model_name)` |
| `LlamaCppEngine` (`InferenceProvider`) | `pond-inference` | `from_model_name(model_id)`, cached at load |
| `PondAgent`, `OllamaInferenceProvider` | `pond-agent` (quarantined) | provider's capabilities; name heuristic |

### `GooseAdapter`

The cache (`model_capabilities: Mutex<ModelCapabilities>`) is refreshed in the provider-swap branch
of `ensure_provider_current` from `model_traits::capabilities`, which `LocalInferenceLlmAdapter` also
calls, so a chat and a side call cannot disagree about a model. It starts from the name heuristic
and overrides it from evidence:

| Field | Source |
|---|---|
| `vision` | `model_traits::vision_declared(provider, model, data_dir)` (below) |
| `thinking` | `model_traits::model_reasons`: the GGUF's template (thinking selectable, or a reasoning marker); a `.litertlm` file's own answer through goose's profile; else the name |
| `tool_calling` | `model_traits::model_uses_native_tools`: the GGUF probe; the `.litertlm` profile; else the name |
| `structured_output` | the `.litertlm` profile's `supported_parameters` (`response_format`); `true` for other `local` / `gguf` models |
| `context_window_tokens` | `model_traits::trained_context_window`: the GGUF header's trained window, or the `.litertlm` file's maximum context, when present |

A `.litertlm` model's traits come from goose's `local_inference::model_profile`, which asks the
LiteRT-LM library and reads the file; `model_traits` keeps one per file version and logs it once
(`read this model's profile from its backend`). When the backend cannot describe the model yet, the
name answers, except for pictures. Everything that is not a local model falls back to the name
heuristic.

`capabilities()` returns the cache with `thinking`, `vision` and `audio_input` forced to `false` in
voice mode.

Until the first swap the cache holds `ModelCapabilities::default()`, so the prompt-building path
does not read it: `thinking_section_applies` and `vision_section_applies` call `model_traits` and
`model_supports_vision` directly. Reading the cache there would move the KV-cached static prefix
between turn one and turn two.

**Vision.** `model_traits::vision_declared` reports what is declared, not what is downloaded:

| Provider | Rule |
|---|---|
| `local`, `gguf` | `vision_encoder::declaration` -> `device_budget::vision_declaration`: the model must pair with an encoder (`encoder_for_model`, by name or GGUF header); on a budgeted device such as the Orin the encoder must also fit and be listed in `DEVICE_MEASURED_VISION`, which is empty, so budgeted devices declare no vision. |
| `local`, `.litertlm` | the profile's `modalities.input` lists `image`. The backend sends text only, so none does yet, whatever encoders the file carries; never the name. |
| `mesh`, `mistralrs` | never; both would drop a picture silently |
| any other | `name_implies_vision` |

Whether the encoder bytes are present and verified is `vision_state`, served as
`GET /api/v1/models/vision-status` (below); it never feeds the prompt.

---

## What each flag drives

| Flag | Effect |
|---|---|
| `thinking` | `thinking_mode` setting (`auto` / `on` / `off`): `on` and `off` decide outright; `auto` asks `model_traits::model_reasons` on the Goose path and `capabilities().thinking` in the mistral.rs agent and `pond-agent`. On the Goose path voice turns are always `false`, for both the `{%- if thinking_enabled %}` prompt section and the engine's `enable_thinking`. Reasoning frames stream only when `show_thinking` is on and the turn is not voice (`reasoning_frames_enabled`). |
| `vision` | The `<vision>` section (`prompts::vision_capability_section`) is applied to the template before rendering when `vision_section_applies` (not voice, and declared), so it sits inside the KV-cached static prefix. Images ride `ChatMessage.images` and reach Goose through `Message::with_image`. The desktop uses `capabilities.vision` only as a fallback gate when vision-status cannot be read. |
| `context_window_tokens` | The last rung of `ContextGovernor` (`ContextInputs.capability_window`), supplied by the REST chat telemetry path, the mistral.rs agent and `pond-agent`. `GooseAdapter`'s own budgets do not pass it. See [Token Usage Tracking](./token_tracking.md#31-contextgovernor). |
| `tool_calling` | Read by `pond-agent`: tool definitions go through the chat template when true, and are listed in the system prompt otherwise. On the Goose path a GGUF's tool mode is set separately when it is registered (`model_traits::tool_mode_for_gguf`). |
| `audio_input`, `structured_output` | Reported only; nothing branches on them. |

---

## REST API

### `GET /api/v1/models/capabilities`

`get_model_capabilities` serialises `state.agent.capabilities()`:

```json
{
  "thinking": true,
  "vision": true,
  "audio_input": false,
  "context_window_tokens": 131072,
  "structured_output": true,
  "tool_calling": true
}
```

There is no window-source field: `WindowSource` is not exposed over HTTP.

### `GET /api/v1/models/vision-status`

`{ model, state, size_bytes, message }` for the active chat model. `state` is pond-core's
`EncoderState`, tagged by `kind`: `unknown`, `not_declared`, `not_on_this_device`, `absent`,
`verifying`, `downloading`, `ready`, `failed`, `blocked`. `message` is the household sentence for it.
It is a pure read, polled by the desktop.

The chat routes refuse a picture turn before anything is saved: 409 with `code` `vision_unsupported`
or `vision_not_ready` (`vision_encoder::refusal_for`).

Model list rows carry a server-computed `reads_images` (`model_views::gguf_vision`).

---

## Frontend

- **Chat (`sections/Chat.tsx`, `hub/views/ChatHub.tsx`)**: `useVisionStatus` gates attaching
  pictures; `ImageSupportStatus` shows the reason when the person reaches for the paperclip or
  pastes. Before vision-status is known, `getModelCapabilities().vision` is the fallback.
- **Settings**: `thinking_mode` and `show_thinking` in `pond-desktop/src/settings/catalogue.ts`.
- **Per-turn footer** (`TurnStatsFooter`): `ctx used/limit` from the SSE `turn_stats` frame.

The Models pages (`sections/Models.tsx`, `hub/views/settings/Models.tsx`) render no capability
badges.

---

## Related documents

- [Token Usage Tracking](./token_tracking.md): the counting side and the governor's window.
- [PAI-3 Context governor](./pai/03-context-governor.md): the window precedence.
- [Agent Pipeline](./agent_pipeline.md): how a turn flows through the agent.
- [Components](./components.md): port trait definitions.
- [Inference Optimization](../developer/inference_optimization.md): platform-specific settings.
