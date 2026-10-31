# Prompt System

How the system prompt for a chat turn is built, stored and changed.

## Per-turn assembly (the Goose path)

Every chat turn runs through `GooseAdapter` (`crates/pond-adapters-goose/src/goose_agent.rs`).
Before each turn it fetches, in parallel: the template named by `settings.prompt_style`, the
active prompt extras, the active user skills, and (when `agent_memory_inject` is on and the
speaker may see personal data) candidate memories. Then:

1. **Template.** `PromptTemplateRepository::get(prompt_style)`, falling back to the built-in
   `balanced` content when the row is missing. `settings.custom_system_prompt`, when set,
   replaces the template entirely.
2. **Vision section.** For a model that can see, `apply_vision_section` appends
   `pond_core::prompts::vision_capability_section(compact)` to the template before rendering.
3. **Render.** `render_jinja_template` (Tera) with the variables below. With
   `prefix_cache_prompt` on (the default), `build_prompt_partition`
   (`crates/pond-core/src/models/services/prompt_builder.rs`) splits the result into:
   - a **static prefix**: the rendered template with date and time blanked. It is hashed, and
     `override_system_prompt` runs only when the hash changes, so the engine's KV cache survives
     between turns;
   - a **dynamic suffix**: `CURRENT CONTEXT` date and time, profile lines and
     `prompt_addendum`, which rides the user message rather than the system prompt.

   With it off, `build_system_prompt_from_template_full` builds the whole prompt every turn.
4. **Extension notes.** Active prompt extras (`render_extension_notes`) and a one-line-per-skill
   list (name and description) are appended by the provider shim
   (`crates/pond-adapters-goose/src/provider_shim.rs`) as `<extension-notes>` blocks, outside
   the prefix hash. A skill's full text is fetched by the model on demand through
   `giap-device__load_skill`.
5. **Memories.** Ranked and token-budgeted memory lines ride the user message's
   `<system-context>`, never the system prompt, so the prefix stays stable.

`GIAP_CAPTURE_PAYLOAD=<dir>` writes the final payload of every provider call to disk; see the
[debug guide](debug_guide.md).

## CLI chat

`pond-server chat` (and `chat --voice`) builds its prompt once at start:
`$DATA_DIR/prompts/system.md` if that file exists (plain `{{key}}` substitution through
`render_template`), else `build_system_prompt(&settings)`, which uses `custom_system_prompt` or
the built-in constant for `prompt_style`. The REST and desktop paths never read
`prompts/system.md`.

## Built-in templates

`BUILTIN_PROMPT_TEMPLATES` in `crates/pond-core/src/prompts.rs` is the one table of built-ins.
`prompt_style` accepts only these four (`PROMPT_STYLES`, enforced by settings validation):

| `prompt_style` | Character |
|---|---|
| `balanced` (default) | Warm, practical, complete behaviour rules |
| `concise` | Minimal, action-first |
| `technical` | Precise, tool-aware, exact values and cited sources |
| `warm` | Conversational, family-friendly |

All four share one flat tag skeleton: `<identity>`, `<instructions>`, `<tool-usage>`,
`<tool-failure>`, `<context-handling>`, `<memory-rules>`, `<output-quality>`, plus the
conditional `<thinking>` and `<voice-mode>`.

Built-ins are seeded by `pond-server setup` (`insert_if_absent`) and reseeded on every `serve`
start (`seed_system_template`). A row a user has saved is marked `is_customized` and the reseed
leaves it alone; `FACTORY_VERSION` (in `user_data/domain/prompt_template.rs`) records which
generation of the built-ins a row came from. `POST /api/v1/prompts/{name}/reset` or
`pond-server prompts reset <name>` restores the factory text.

## Variables and conditions

The variable list is the `ctx.insert` calls in `render_jinja_template`:

| Variable | Source |
|---|---|
| `assistant_name`, `user_name`, `timezone` | Settings, sanitized, max 50 chars |
| `personality` | `assistant_personality`, sanitized, max 200 chars |
| `location` | Always empty (the weather tools already know the home); kept so old templates render |
| `current_date`, `current_time` | Blank in the static prefix; they go to the dynamic suffix |
| `voice_mode` | The CLI `--voice` instance flag or the request's `voice_mode` |
| `thinking_enabled`, `reasoning_budget_words` | `thinking_mode` (`auto` reads the model file), off in voice; `reasoning_effort` sets the word budget |
| `compact_prompt` | Small context windows get the short tier |
| `tools_offered`, `has_tools`, `tools`, `native_tools_json` | Whether tools are offered at all (`GIAP_NO_TOOLS` turns them off), prose lines for external MCP tools, and whether the chat template already carries the tools JSON (`local`, `gguf`, `mistralrs`) |
| `atypical_speech` | The speaker's profile |

A Tera error falls back to plain `{{key}}` substitution. `sanitize_field` turns control characters
into spaces, collapses whitespace and truncates; `custom_system_prompt` is cut at 4,000 chars and
`prompt_addendum` at 500.

### The `<vision>` section

Appended only when `vision_section_applies` holds: not in voice (the instance flag, so the prefix
cannot flip between turns of one session) and `model_supports_vision`:

- `local` / `gguf`: the model's declared vision encoder (`vision_encoder::declaration`), not
  whether the encoder file is downloaded yet, so a background download does not rewrite the
  prefix mid-session;
- `mesh`, `mistralrs`: never;
- HTTP providers: `ModelCapabilities::name_implies_vision`, a deliberately narrow name rule.

A turn that carries an image before the encoder is ready is refused (409 `vision_not_ready` /
`vision_unsupported`) rather than handed to a model that cannot decode it. A
`custom_system_prompt` does not get the section.

## Managing templates, extras and skills

| Surface | Routes | CLI |
|---|---|---|
| Templates | `GET /api/v1/prompts`, `GET\|PUT\|DELETE /api/v1/prompts/{name}`, `POST /api/v1/prompts/{name}/reset` | `pond-server prompts list\|show\|reset` |
| Extras | `GET\|POST /api/v1/agent/extras`, `DELETE /api/v1/agent/extras/{key}` | `pond-server agent extras` (list) |
| Skills | `GET\|POST /api/v1/skills`, `PUT\|DELETE /api/v1/skills/{id}` | `pond-server skills list\|add\|toggle\|remove` |

- `PUT /prompts/{name}` creates or edits a template. Built-ins (`is_system`) cannot be deleted.
  A template under a new name is stored but cannot be selected: `prompt_style` accepts only the
  four built-ins. To change the wording, edit a built-in or set `custom_system_prompt`.
- `POST /agent/extras` upserts by key; `instruction` is required, `active` defaults to `true`,
  `sort_order` to 0. Extras are validated and capped (`MAX_ACTIVE_EXTRAS`,
  `MAX_TOTAL_INSTRUCTION_CHARS` in `user_data/domain/prompt_extra.rs`); one over the cap is
  skipped with a warning.
- An extra with a `profile_id` is that member's own (`/api/v1/profiles/{id}/notes`, and the setup
  interview's drafts). It is never in the system prompt: `profile_context_for` loads it only when
  the turn is that member's, and the adapter renders it as `<member-notes>` in the turn's
  `<system-context>`, once per conversation (`member_notes_block`). The household's active extras
  and the speaker's share one cap.
- `POST /skills` takes `name`, `content`, optional `description` (shown to the model every turn)
  and `icon`; `PUT /skills/{id}` patches any of those or `active`.

## Agent settings that shape the turn

| Field | Default | Effect |
|---|---|---|
| `agent_goose_mode` | `auto` | Goose's mode: `auto`, `chat`, `smart` |
| `agent_max_turns` | `50` | Max provider calls per request; `0` is uncapped |
| `agent_timeout_secs` | `300` | Seconds without a stream event before a turn aborts |
| `agent_memory_inject` | `true` | Inject memories into the user message |
| `agent_memory_limit` | `5` | How many memory fragments to inject |
| `prefix_cache_prompt` | `true` | Static prefix plus dynamic suffix (above) |

## Key files

| File | Role |
|---|---|
| `crates/pond-core/src/prompts.rs` | `PROMPT_*`, `BUILTIN_PROMPT_TEMPLATES`, `vision_capability_section`, `render_jinja_template`, `build_system_prompt*`, `sanitize_field` |
| `crates/pond-core/src/models/services/prompt_builder.rs` | `build_prompt_partition` |
| `crates/pond-core/src/user_data/{domain,ports}/{prompt_template,prompt_extra,skill}.rs` | Domain types and repository traits |
| `crates/pond-infra/src/sqlite_{prompt_template,prompt_extra,skill}.rs` | SQLite repositories |
| `crates/pond-infra/migrations/system/` | `0009_prompt_templates`, `0010_prompt_extras`, `0011_skills`, `0031_prompt_template_customized`, `0048_prompt_template_factory_version`, `0050`/`0051` skill description and icon |
| `crates/pond-adapters-goose/src/goose_agent.rs` | Per-turn assembly, vision and thinking gates |
| `crates/pond-adapters-goose/src/provider_shim.rs` | Enforces the system prompt and tools GIAP chose; appends extension notes; payload capture |
| `crates/pond-server/src/main.rs` | `run_setup` seeds built-ins; `serve` reseeds them; CLI chat's file override |

`crates/pond-core/src/system_prompts_template.md` is a design sketch that no code reads.
