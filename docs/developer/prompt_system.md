# GIAP Prompt System

## Overview

The prompt system controls the system prompt sent to the LLM on every chat turn. It is designed to be:

- **Privacy-first** — every template emphasises on-device operation and no data egress
- **Voice-aware** — all templates forbid Markdown and pipeline control tokens
- **DB-editable** — built-in templates are seeded to the database at `run_setup()` and editable via REST API or direct SQL; no prompt strings are hard-coded in the binary at runtime
- **Safe** — all user-supplied strings are sanitized (control chars stripped, length-bounded) before substitution

---

## How Prompts Are Built (GooseAdapter path)

All production chat routes through `GooseAdapter` (`crates/pond-adapters-goose/src/goose_agent.rs`). On every turn:

1. `SettingsRepository::get()` — loads current settings
2. `PromptTemplateRepository::get(settings.prompt_style)` — fetches the active template from DB
3. `build_system_prompt_from_template(&settings, &template_content)` — renders placeholders
4. `agent.override_system_prompt(rendered)` — injects as the Goose agent's system prompt
5. Active `PromptExtra` records → `agent.extend_system_prompt(key, instruction)` for each
6. Active `UserSkill` records → `agent.extend_system_prompt("skill:<name>", content)` for each
7. If `settings.agent_memory_inject = true` → recent memory fragments also injected as an extra

### Fallback for CLI chat

The CLI `chat` command still uses `build_system_prompt(&settings)` (`pond-core/src/prompts.rs`) as a direct fallback. This is a Rust-code path, not a DB query.

---

## Priority Chain (CLI / file-override path)

| Priority | Source | How |
|---|---|---|
| 1 | File at `$DATA_DIR/prompts/system.md` | `render_template()` with all vars — deployment/sysadmin overrides |
| 2 | `Settings.custom_system_prompt` | `render_template()` with all vars — per-user full override stored in DB |
| 3 | Active template from `prompt_templates` DB | Selected by `Settings.prompt_style` |
| 4 | `SYSTEM_PROMPT` constant | Hardcoded fallback — used when Settings unavailable |

---

## Built-in Prompt Styles

Four built-in templates are seeded at `run_setup()` via `PromptTemplateRepository::insert_if_absent()`. They are stored in `prompt_templates` with `is_system = true` (not deletable via API, but always editable). The source constants are in `pond-core/src/prompts.rs` as `pub const PROMPT_*`.

| Style | `prompt_style` value | Audience | Character |
|---|---|---|---|
| Balanced | `"balanced"` | Default — most households | Warm + practical, full safety rules, voice-safe |
| Concise | `"concise"` | Power users | 1-sentence replies, action-first, minimal |
| Technical | `"technical"` | Developers | Narrates tool use, verbose, shows reasoning |
| Warm | `"warm"` | Families | Conversational, friendly, no jargon |

All four share the same safety rules (door unlock confirmation, unknown device response, external network gate).

---

## Managing Templates via REST API

```http
# List all templates
GET /api/v1/prompts

# Get a specific template
GET /api/v1/prompts/balanced

# Edit a built-in template (content updated; is_system stays true)
PUT /api/v1/prompts/balanced
Content-Type: application/json
{ "content": "You are {{assistant_name}}. Be very brief.", "description": "Ultra-concise" }

# Create a user-defined template
PUT /api/v1/prompts/swahili
Content-Type: application/json
{ "content": "Wewe ni {{assistant_name}}. Jibu kwa Kiswahili.", "description": "Swahili mode" }

# Delete a user-defined template (fails for is_system = true templates)
DELETE /api/v1/prompts/swahili

# Switch active template
PUT /api/v1/settings
{ "prompt_style": "swahili" }
```

---

## System Prompt Extras

Per-key extra instructions are stored in `prompt_extras` and injected on every turn via `agent.extend_system_prompt(key, instruction)`. Useful for adding rules without editing a full template.

```http
# Add or update an extra
POST /api/v1/agent/extras
{ "key": "language", "instruction": "Always respond in French.", "active": true, "sort_order": 10 }

# List all extras
GET /api/v1/agent/extras

# Disable without deleting
POST /api/v1/agent/extras
{ "key": "language", "active": false }

# Delete
DELETE /api/v1/agent/extras/language
```

---

## User Skills

Skills are named markdown instruction blocks stored in `user_skills`. Each active skill is injected as `skill:<name>` extra on every turn.

```http
POST /api/v1/skills
{ "name": "home_automation", "content": "When user asks about lights, call giap__list_registered_devices first." }

GET /api/v1/skills
PUT /api/v1/skills/{id}  { "active": false }
DELETE /api/v1/skills/{id}
```

---

## Template Variables

All templates (built-in, DB, file overrides) support these `{{placeholder}}` variables:

| Variable | Source | Notes |
|---|---|---|
| `{{assistant_name}}` | `Settings.assistant_name` | Sanitized, max 50 chars |
| `{{user_name}}` | `Settings.user_name` | Sanitized, max 50 chars |
| `{{personality}}` | `Settings.assistant_personality` | Sanitized, max 200 chars |
| `{{timezone}}` | `Settings.timezone` | Sanitized, max 50 chars |
| `{{location}}` | `Settings.weather_location_name` | Either `"\nLocation: <name>."` or `""` |
| `{{prompt_addendum}}` | `Settings.prompt_addendum` | Appended after the main template |

---

## Conditional Sections

The four built-in styles share one ordered tag skeleton. Some sections only render when their gate is true:

| Section | Gate | Source |
|---|---|---|
| `<home-devices>` | `has_home_devices` | `DeviceRegistry` count > 0 |
| `<thinking>` | `thinking_enabled` | `Settings.thinking_mode` + model capability, forced off in voice mode |
| `<voice-mode>` | `voice_mode` | CLI `--voice` or `ChatRequest.voice_mode` |
| `<canvas-mode>` | `canvas_mode` | `ChatRequest.canvas_mode` |
| `<vision>` | model is vision-capable **and** not voice mode | appended by `GooseAdapter`, see below |

### `<vision>` — the multimodal self-model

Nothing else in the prompt tells the model it can see, and the omission is not theoretical: with a fully encoded image in context, Gemma-4-E4B answered *"I cannot directly describe the content of an image you provide. I am a text-based assistant."* The section states the capability outright and draws the line between an image **attached to the message** (directly visible — just look at it) and a **live camera frame** (needs a `giap-vision` tool). The same model, asked "what do you see?" with an attachment present, had offered camera frames instead.

Mechanics, which differ from the other conditional sections:

- The text is a constant in `pond-core/src/prompts.rs` (`vision_capability_section(compact)`), **not** a `{% if %}` block in the templates. `GooseAdapter::apply_vision_section()` appends it to the template string *before* Tera renders it, so it lands inside `static_prefix` and `prefix_hash` covers it automatically.
- It is Jinja-free by contract (a test asserts no `{{` / `{%`), so it renders verbatim through any template.
- It has a compact variant, selected by the same `compact_prompt` tier as the rest of the prompt.
- The gate is `GooseAdapter::vision_section_applies(provider, model, voice)`. Rendering the section for a text-only model would manufacture a hallucination, so it is never on by default:
  - **Model.** `model_supports_vision`: for `local` / `gguf` pond-core's device-aware declaration (`device_budget::vision_declaration`: a pinned encoder exists for this family and qat-ness, and on a budgeted device it fits without costing the window and has been measured there); `mesh` and `mistralrs` never, since both would drop the picture; for HTTP providers `ModelCapabilities::name_implies_vision`, a name rule deliberately narrower than the rest of `from_model_name`. On the Gemma 4 family it reaches the same verdict the registry would — `gemma-4-E1B` is the one Gemma 4 with no encoder and is excluded in every spelling, including Ollama's `gemma3n:e1b` — and it recognises the vision models an Ollama install actually serves (`llama3.2-vision`, `qwen2.5-vl`, Ollama's unhyphenated `qwen2.5vl`, `minicpm-v`, `pixtral`, `llava`, `moondream`, …). Unrecognised names stay `false`: a false positive tells a blind model it can see, a false negative only withholds a prompt section.
    Two of its three rules name a known family outright; the third credits any name carrying `vision` / `vl` / `vlm` as a **whole segment**. That marker rule is what covers the long tail of vendors labelling a multimodal build without appearing in any list, and the price is that a name carrying the segment for an unrelated reason (`vision-labs/text-only-7b`) is credited too. Segment matching rather than substring matching keeps that to contrived names; it is accepted, not solved. So: not every rule is a positive identification of a known family.
  - **Voice.** `AgentPort::capabilities()` reports `vision = false` in voice mode, so the section is suppressed there too — otherwise the prompt asserts a capability the adapter denies. Both read the same *instance-level* flag (CLI `--voice`), never `ChatRequest.voice_mode`: that is the only signal `capabilities()` can see, and a per-request gate would flip the static prefix between turns of one session and forfeit the KV cache.
- **Declared, not downloaded.** The vision encoder is ~941 MB and fetched in the background, so "the bytes are on disk" flips mid-session; the model's *declaration* does not. Keying off the download would rewrite the system prefix mid-session and forfeit the engine's KV prompt-session cache for every turn after it. A turn that actually needs the encoder before it is ready is refused before anything is saved, by the chat routes (409 `vision_not_ready` / `vision_unsupported`, from `GET /models/vision-status`'s state) and again by `chat_stream` as a backstop, with the household sentence for the state it is in (downloading, verifying, blocked by Network reach, failed with a retry time), so the model is never handed an image it cannot decode.
- `Settings.custom_system_prompt` is a **full override** and is used instead of the template, so it does not receive this section. A custom prompt owns its whole contents, including any vision wording.

---

## Security

`sanitize_field(s, max_len)` is applied to every user-supplied value before substitution:

1. All ASCII control characters (0x00–0x1F, 0x7F) → replaced with space (prevents newline injection)
2. Whitespace runs collapsed to single space, trimmed
3. Truncated to `max_len` characters

`custom_system_prompt` itself is sanitized with `max_len = 4000` before rendering.

---

## File Override

Drop a `system.md` file at `$DATA_DIR/prompts/system.md` (defaults to `~/.local/share/goose-in-a-pond/prompts/system.md` on Linux). The CLI chat path checks for this file at startup — the REST/GooseAdapter path does not use it.

---

## Agent Settings Fields

Four settings fields control GooseAdapter's agentic loop behaviour:

| Field | Default | Description |
|---|---|---|
| `agent_goose_mode` | `"auto"` | GooseMode: `"auto"`, `"chat"`, or `"smart"` |
| `agent_max_turns` | `50` | Max loop turns per request; `0` = uncapped |
| `agent_memory_inject` | `false` | Whether to inject recent memories into the system prompt |
| `agent_memory_limit` | `5` | How many memory fragments to inject (most recent) |

---

## Key Files

| File | Role |
|---|---|
| `crates/pond-core/src/prompts.rs` | `pub const PROMPT_*`, `vision_capability_section`, `build_system_prompt(&Settings)`, `build_system_prompt_from_template`, `sanitize_field`, `render_template` |
| `crates/pond-core/src/user_data/domain/settings.rs` | `prompt_style`, `custom_system_prompt`, `prompt_addendum`, `agent_*` fields |
| `crates/pond-core/src/user_data/ports/prompt_template.rs` | `PromptTemplateRepository` trait |
| `crates/pond-core/src/user_data/ports/prompt_extra.rs` | `PromptExtraRepository` trait |
| `crates/pond-core/src/user_data/ports/skill.rs` | `UserSkillRepository` trait |
| `crates/pond-core/src/user_data/domain/prompt_template.rs` | `PromptTemplate` domain type |
| `crates/pond-core/src/user_data/domain/prompt_extra.rs` | `PromptExtra` domain type |
| `crates/pond-core/src/user_data/domain/skill.rs` | `UserSkill` domain type |
| `crates/pond-infra/src/sqlite_prompt_template.rs` | `SqlitePromptTemplateRepository` |
| `crates/pond-infra/src/sqlite_prompt_extra.rs` | `SqlitePromptExtraRepository` |
| `crates/pond-infra/src/sqlite_skill.rs` | `SqliteSkillRepository` |
| `crates/pond-infra/migrations/system/0009_prompt_templates.sql` | DB schema |
| `crates/pond-infra/migrations/system/0010_prompt_extras.sql` | DB schema |
| `crates/pond-infra/migrations/system/0011_skills.sql` | DB schema |
| `crates/pond-adapters-goose/src/goose_agent.rs` | DB-driven prompt injection per turn |
| `crates/pond-api/src/routes.rs` | REST endpoints for prompts, extras, skills |
| `crates/pond-server/src/main.rs` | `run_setup()` seeds built-in templates via `insert_if_absent()` |
