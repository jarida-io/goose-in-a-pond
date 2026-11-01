# PAI-6 — Multi-agent orchestration

Requirement: *multi-agent orchestration.* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: [PAI-1](./01-identity-and-profile-boundaries.md) (whose data a subagent may touch),
[PAI-2](./02-privacy-and-security-guardrails.md) (what it may do),
[PAI-3](./03-context-governor.md) and [PAI-4](./04-smart-compaction.md) (what it may spend).

Verified against code **2026-08-07**, in a checkout with the submodule initialised at
`29ef609c` (`heads/main`). Every Goose-side claim below was read out of `goose/crates/goose/src/`
in this tree; the submodule is uninitialized in a fresh clone, so run
`git submodule update --init --recursive` before re-checking (CI checks out the pinned submodule the
same way).

> Status (2026-10-28): P1-P8 are landed. Long-running tasks (section 8) and agents as recipes
> (section 9) landed P9-P11, P14-P17 and P19 on 2026-10-28; P12 is partial, P13 is open and P18 is
> deferred (8.6, 9.4). P4's `context_fraction` shrink reaches only
> `plan_replay` since 2026-09-11, when goose took back trimming the live conversation; the device hold
> half is unaffected. Programme summary: [checklist section 0](./00-checklist.md).

> **Read this before implementing any phase.** The 2026-08-07 re-verification found **six**
> current-state claims in this document that were false, three of which would have made an
> implementer write code that does not compile or does nothing. They are corrected in place and
> called out where they were. The largest is in section 1.2: **`run_subagent_task` is not
> callable from `pond-adapters-goose`.** P2 is a fork-patch decision before it is a coding task.
>
> **PAI-6 is code-complete as of 2026-08-11: P1-P8 all landed.** The two things it is still owed are
> a MEASUREMENT and a CALL SITE, both named in their phases' stamps — section 7's integration
> comparison (parent context growth with a delegation versus inline, which needs an Orin), and a
> production caller for `Orchestrator::cancel_children_of`, without which a deleted session does not
> stop a background run. Neither is a design question.

---

## 1. What is true today

### 1.1 GIAP has the vocabulary and none of the machinery

- **`DelegatingAgent`** — `pond-core/src/shared/services/delegation.rs`, **295** lines (this said
  293). An `Agent` that holds `Vec<(MatchFn, Arc<dyn Agent>)>` plus a fallback and routes by first
  match on message text, preserving `session_id`. Its own module doc (the paragraph beginning
  "NOTE: this is a neutral routing primitive") warns against wiring it with hardcoded keyword
  matchers. It is a **switch, not a spawner** — no parallelism, no aggregation, no task lifecycle.
  Deliberately unwired: `grep -rn DelegatingAgent crates/` finds it only inside its own file.

  **It is not a base for `Orchestrator`, and P1 did not treat it as one.** Its unit of dispatch is
  a pre-constructed `Arc<dyn Agent>`, so it can select between children but never *derive* one from
  a parent, which is the only thing orchestration needs. It forwards the `AgentRequest` unchanged,
  which hands the child the parent's `profile_scope` and `session_id` verbatim — invariant 1 and
  invariant 4 both inverted. And its selection input is message text, which is the keyword
  classification the working agreement forbids. Leave it alone; do not refactor it.
- **`prompts/subagent_system.md`** — GONE (2026-08-06). It was a complete, GIAP-flavoured subagent
  system prompt registered as a Goose override by `giap_prompts.rs`, and GIAP never rendered or
  spawned it: the live prompt path is pond-core's `build_prompt_partition`. The whole of
  `giap_prompts.rs` and `src/prompts/` (12 templates) is deleted, so there is no longer a
  GIAP-authored subagent prompt to point at if orchestration is ever built.
- **`AgentRecipe`** — `user_data/domain/recipe.rs`, **six** fields:
  `{ id, name, description, yaml, active, created_at }` (this said five; same class of miscount as
  the "Profile has six fields not five" correction in the checklist's 2026-08-04 audit).
  `POST /api/v1/recipes/{name}/run` parses the YAML via `routes.rs :: RecipePrompt` — cite it by
  symbol, the old `routes.rs:9614` citation is dead — a four-field serde struct that reads two;
  the `goose::recipe::Recipe` call was the only `goose::` reference in `pond-api` and was removed
  to keep that crate framework-free. It extracts `prompt` or `instructions` and runs it through the
  **same chat-stream pipeline as an ordinary user message**. A recipe today is a stored prompt.

  **The table is `agent_recipes`, not `recipes`** (migration `0012_recipes.sql` names the file
  after the concept and the table after the type). Anyone implementing "roles live in the recipes
  table" from the old wording writes `SELECT ... FROM recipes` and gets `no such table` — or, worse,
  adds a migration creating one, which is exactly the persistence P1 forbids.
  `SqliteRecipeRepository::upsert` is `INSERT OR REPLACE … datetime('now')`, so editing a role
  **resets its `created_at`**. Do not build anything that trusts that ordering.
- **`ModelRouter`** — GONE. `model_router.rs` was never declared by a `mod` statement, so it was
  never compiled and its six tests never ran; three of them asserted routing behaviour its
  `complete` contradicted. Per-role dispatch, if wanted, starts from the live provider hot-swap
  seam in `pond-api`'s `AppState`, not from this file.
- **`UserSkill`** — named Markdown injected every turn, deliberately placed outside the KV-cache
  prefix hash. It no longer travels through `Agent::extend_system_prompt`: Goose rebuilt that into
  an appendix the provider shim discarded, and the entry was never removed when a skill was
  deactivated. The shim's per-session `turn_appendix` is now the only delivery path.

No `spawn_subagent`, no task queue, no worker pool, no parallel fan-out, no inter-agent messaging,
no result aggregation exists anywhere in `crates/`.

### 1.2 Goose has real machinery, and GIAP strips it

The `strip_list` in `goose_agent.rs` (grep the symbol; the old `:2466-2478` citation now lands in
`resolve_session_tool_groups`, which is unrelated) removes ten builtins from every session unless
the user explicitly added them: `developer`, `computercontroller`, `extensionmanager`, `todo`,
`apps`, `analyze`, **`summon`**, `summarize`, **`orchestrator`**, `tom`.

**There are two hardcoded copies of those names in that file** — `strip_list`, which is the guard,
and the `is_builtin` closure further down, which carries the same ten plus `default` and
`suggestions`. Only the first enforces anything. A canary test that pins one proves nothing about
the other; pin both.

What is being stripped:

| Goose surface | What it provides |
|---|---|
| `goose/crates/goose/src/agents/subagent_handler.rs` | `run_subagent_task(SubagentRunParams { config, recipe, task_config, return_last_only, session_id, cancellation_token, on_message, notification_tx })` — a complete child agent loop with cancellation and a notification channel. **`pub(crate)`. See below.** |
| `.../subagent_task_config.rs` | `TaskConfig { provider, model_config, parent_session_id, parent_working_dir, extensions, max_turns }` — the module is `pub(crate)` but the type is re-exported, so this one IS reachable |
| `.../platform_extensions/summon.rs` | `load` (list/load subrecipes, recipes, agents) and `delegate` (ad-hoc or source-based subagent, `async: true` for background, then `load(taskId)` to collect) |
| `.../platform_extensions/orchestrator.rs` | `list_sessions`, `view_session`, `start_agent`, `send_message`, `interrupt_agent` over Goose's session manager |
| `.../subagent_execution_tool/` | **serde types only.** `mod.rs` is three lines and `notification_events.rs` holds `TaskStatus`, `TaskExecutionNotificationEvent`, `TaskExecutionStats`, `TaskInfo`, `FailedTaskInfo`. This row used to say "task execution plumbing"; there is no executor, no queue and no scheduler here. P8 has nothing to reuse *(confirmed by P8 on 2026-08-11: it reused none of it, and needed none of it — a background run is `tokio::spawn` over the same `drive_run` a synchronous one awaits)* |
| `.../recipe/` | full Recipe model, validation, templating, subrecipes |

#### CORRECTION (2026-08-07): `run_subagent_task` is NOT callable, and this changes P2

`goose/crates/goose/src/agents/mod.rs` declares:

```rust
pub(crate) mod subagent_handler;
pub(crate) mod subagent_task_config;
```

and its re-export block exports exactly `subagent_handler::SUBAGENT_TOOL_REQUEST_TYPE` and
`subagent_task_config::TaskConfig`. So `run_subagent_task`, `SubagentRunParams` and
`OnMessageCallback` are **crate-private to `goose`** and unreachable from `pond-adapters-goose`.
The only caller in the whole Goose workspace is `platform_extensions/summon.rs` — the extension
GIAP strips.

This document previously closed section 3.1 with "the part worth having is `run_subagent_task`,
which is callable directly." It is not. P2 must choose, **before writing code**, between:

1. **A sixth GIAP fork patch** — two `pub(crate)` → `pub` plus a re-export — staged on
   `jarida-io/Goose` per `docs/goose-patch-management.md`, with the CI fork-branch-tip bump. Note
   that `goose-patch-management.md` already carries **five** rows while `AGENTS.md` still describes
   the patch set as two; that staleness is P2's to clear if it takes this route.
2. **Reimplementing the child loop** over the public `Agent` API (`Agent::with_config`,
   `update_provider`, `add_extension`, `apply_recipe_components`, `override_system_prompt`,
   `reply`), which is entirely possible from re-exported symbols and costs ~120 lines in
   `pond-adapters-goose` that duplicate `get_agent_messages` — and which is the only route that
   lets GIAP set the child's system prompt (see section 3.2).

**P2 took route 2 (2026-08-07).** `GooseAdapter::run_child_agent` is that loop. The patch set
stays at five and the child's system prompt is GIAP's own. Two things fell out of owning it that
the fork route would not have given: an extension that fails to load is a hard error rather than
Goose's `debug!`-and-swallow, and the drain keeps `as_concat_text()` only, so a child's
`MessageContent::Thinking` cannot reach the parent as its answer.

So the expensive, fiddly part — spawning a child agent with its own conversation, capping its
turns, cancelling it — exists, is switched off, **and is behind a visibility wall**. What is
genuinely free is the structure: `Agent::with_config` builds an `ExtensionManager` with **no**
extensions and nothing auto-loads defaults into it, so a child starts with an empty tool surface
and gets exactly what the adapter puts in `TaskConfig.extensions`. Invariants 1, 2 and 6 are
achievable by construction there — provided the extension list is built from GIAP's own
`registered_extensions()` and never from `EnabledExtensionsState::extensions_or_default`, whose
fallback re-arms every `default_enabled` platform extension including `summon`.

#### Four more Goose-side facts an implementer needs, and will otherwise find the hard way

- **`available_tools: vec![]` means ALL TOOLS.** `ExtensionConfig::Builtin.available_tools` is a
  real per-extension allowlist enforced twice (listing, and inside `dispatch_tool_call`), but its
  predicate is `available_tools.is_empty() || available_tools.contains(&tool_name)`. GIAP's own
  `builtin_extension_config` helper passes `vec![]`. Reusing it for a child is a one-line
  scope-widening default.
- **The provider shim does not veto a subagent's tools.** `GiapProviderShim` resolves its per-turn
  allow-set from `session_context::current_session_id()` via `ShimControls::existing_session`,
  which does not create entries. A child runs under its own Goose session id, for which GIAP has
  published nothing, so `enforce_tools(tools, &None)` is a no-op — and because the turn appendix is
  also `None`, the `system_appendix_dropped` warning does not fire either. It fails **silently**.
- **`TaskConfig { max_turns: None, .. }` panics.** `build_subagent_prompt` does
  `.expect("TaskConfig always sets max_turns")`. GIAP will construct `TaskConfig` by struct literal
  (because `TaskConfig::new` reads Goose's global `Config`), so it must always pass `Some(n)`.
- **A cancelled child returns `Ok`, and so does one that ran out of turns.** The reply loop breaks
  and returns `Ok(partial_text)`; an exhausted `max_turns` yields the literal `MAX_TURNS_MESSAGE`
  string as the "result". A test asserting "cancel works" by checking for an `Err` is vacuous.
  PAI-6 P1's `TaskStatus` has `Cancelled` and `TurnBudgetExhausted` as distinct variants, and
  `TaskRun::result_for_parent` returns `None` for both, precisely so the adapter has somewhere
  correct to put the distinction.

---

## 2. The gap

GIAP has one agent that does everything in one context window, with one tool surface, at one
priority. Anything that needs a different persona, a different tool scope, a different model, or an
isolated context has nowhere to run.

---

## 3. Recommendation: reuse the executor, own the policy

The question is whether to build orchestration in `pond-core` or re-enable Goose's. **Neither
alone. The split is: `pond-core` owns *what may run and under what limits*; the Goose adapter owns
*how a child agent is executed*.**

### 3.1 Why not simply re-enable `summon` and `orchestrator`

1. **They are coding-agent shaped.** `TaskConfig` carries a `parent_working_dir`; `summon`
   discovers "sources" by scanning the filesystem; the `delegate` tool description advises
   "partition files strictly — no two delegates touch the same file". None of that describes a home
   assistant, and it is the tool text a 2-4B on-device model will read and try to act on.
2. **They bypass every boundary this programme is building.** They do not know about GIAP's
   tool-group narrowing (`mcp/domain/tool_group.rs`), the always-on draft gate, or profile scope.
   `orchestrator.start_agent` would hand a fresh agent the full 61-tool surface that Phase D spent
   an entire phase narrowing to 17.
3. **They are surface, and surface is the part that must survive a fork swap.** The programme's
   invariant 5 exists for this: anything that would need rewriting if Goose were replaced belongs in
   `pond-core`.

Keeping them stripped costs nothing. What is worth having is the child agent loop — and reaching it
is a decision, not a call; see the correction in section 1.2.

### 3.2 The domain — AS LANDED (P1, 2026-08-07)

`pond-core/src/shared/domain/orchestration.rs` + `pond-core/src/shared/ports/orchestrator.rs`.

**Not `pond-core/src/agents/`**, which this document used to propose. `lib.rs` declares exactly
six top-level modules and each is a bounded context; orchestration spans models (provider, window),
mcp (tool groups) and user_data (recipes, profiles), which is the definition of `shared/` in this
tree — where `chat`, `delegation`, `session_summary` and `egress` already live.

```rust
pub struct AgentRole { /* private */ }        // name, instructions, tool_groups,
                                              // personal_data, max_turns, context_fraction
pub enum RolePersonalData { Inherit, Deny }   // NO variant widens
pub struct DelegationDepth(u8);               // no Deserialize, no Default, private increment
pub struct DelegationAuthority { /* private */ }   // session_id, scope, tool_groups, depth
pub struct TaskRequest { pub role, pub instructions, pub inputs }  // deny_unknown_fields
pub struct TaskSpec { /* private, derived */ }
pub enum TaskStatus { Queued, Running, Completed, Cancelled, TurnBudgetExhausted, Failed }
pub struct TaskRun { id, role, parent_session_id, status, result, error, started_at, finished_at }

impl DelegationAuthority {
    pub fn root(session_id, scope, tool_groups) -> Self;
    pub fn delegate(&self, role: &AgentRole, request: TaskRequest)
        -> Result<TaskSpec, DelegationRefused>;
}
impl TaskSpec {
    pub fn grants_tool(&self, tool_name: &str) -> bool;
    pub fn child_authority(&self, child_session_id) -> DelegationAuthority;
}

#[async_trait]
pub trait Orchestrator: Send + Sync {
    async fn spawn(&self, spec: TaskSpec) -> Result<TaskRun>;
    async fn poll(&self, task_id: &str) -> Result<Option<TaskRun>>;
    async fn cancel(&self, task_id: &str) -> Result<()>;
    async fn list(&self, parent_session_id: &str) -> Result<Vec<TaskRun>>;
    async fn cancel_children_of(&self, parent_session_id: &str) -> Result<usize>;
}
```

Four differences from the sketch above it, each for a reason:

- **`AgentRole` has no `profile_scope` and no `model_role`.** An absolute `ProfileScope` on a
  stored role is a widening waiting to happen — a role saying `household` would widen a `Guest`
  parent — and which member is speaking is a per-turn fact that a stored config cannot know.
  `RolePersonalData` replaces it: `Inherit` gives the parent's scope exactly, `Deny` drops to
  `Guest`. **There is no value of that type that widens**, so invariant 1's profile axis needs no
  runtime check. `model_role` is deferred to P7, which has to define the type first (see below).
- **`TaskSpec` is derived, never constructed.** Its fields are private, it has no `Deserialize`,
  and the only producer is `DelegationAuthority::delegate`. Everything the child may do is computed
  from the parent and the role.
- **`TaskRequest` — the part the model supplies — carries no authority at all**, and
  `deny_unknown_fields` makes that enforced rather than ignored: a `delegate` call carrying
  `profile_scope`, `tool_groups`, `depth` or `session_id` is a parse error the model can see, not a
  field silently dropped.
- **`spawn` takes no `parent: &SessionRef`.** The spec already carries the parent's session id.
  Passing it twice lets the two disagree, and the one nearer the engine wins — which is how a scope
  gets widened by accident.

`pond-adapters-goose` implements `Orchestrator` — **AS LANDED (P2, 2026-08-07)** it translates a
`TaskSpec` into a `ChildPlan` (system prompt, user message, `Vec<ExtensionConfig>`, `max_turns`)
and drives it over the public `Agent` API. There is no `Recipe` and no `TaskConfig` on the landed
path: both exist only to be consumed by `run_subagent_task`, which is not callable, and
`recipe.extensions` was never read by it anyway. The paragraph below is why.

**Goose's own child loop cannot render a GIAP subagent system prompt.** GIAP's
`prompts/subagent_system.md` was deleted on 2026-08-06 with the whole of `giap_prompts.rs`, and
Goose's own copy — `goose/crates/goose/src/prompts/subagent_system.md`, opening "You are a
specialized subagent within the goose AI framework, created by AAIF" — is rendered
**unconditionally** by `build_subagent_prompt`, which then calls `override_system_prompt` on a
child `Agent` it constructs internally and never returns. There is no seam. The only
caller-controlled input is `Recipe.instructions`, which lands inside that template as
`{{task_instructions}}`. The three options were: accept a hybrid, goose-branded child prompt (the
shim will **not** correct it — its `GOOSE_DEFAULT_MARKER` does not appear in that template); patch
the submodule; or take route 2 from section 1.2 and own the loop. **P2 took the third.** The
child's prompt is `build_prompt_partition(...).static_prefix` — the same call the parent's turn
makes — plus a GIAP-authored delegation envelope naming the role, the turn budget, the depth cap
and the child's exact tool names. A guard asserts the prompt starts with that prefix and carries
none of `goose AI framework` / `AAIF` / `Agentic AI Foundation`.

### 3.3 What is reused rather than rebuilt

| Need | Existing code |
|---|---|
| Role definitions | `AgentRecipe` rows in the **`agent_recipes`** table — role fields ride one GIAP-owned YAML key, `giap_role`, which `RecipePrompt` and Goose's `Recipe` both ignore |
| Reusable instruction blocks | `UserSkill` |
| Per-role tool narrowing | `mcp/domain/tool_group.rs` + `services/tool_selection.rs` — but use `filter_tools_by_groups` (a pure intersection), **never** `select_groups`, whose every failure path widens by design |
| Per-role model | **Nothing.** `ModelRouter` does not exist and never compiled; there is no `ModelRole` type. *(P7 LANDED 2026-08-11 and reused none of it: the type is `ChildModel` in `pond-core`, the mechanism is a second `ModelConfig` on the SAME `Arc<dyn Provider>`, and there is no router. See P7's stamp for why a router would have been the wrong shape.)* |
| Cheap in-process routing | Nothing. `DelegatingAgent` is a dead end — see section 1.1 |
| Child agent execution | **Nothing reusable.** `run_subagent_task` is `pub(crate)`; P2 owns the loop over `Agent::with_config` / `update_provider` / `add_extension` / `override_system_prompt` / `reply`, all of which ARE re-exported |
| Subagent system prompt | **Nothing.** GIAP's is deleted; Goose's is not substitutable. P2 builds it through `build_prompt_partition` — see section 3.2 |
| Concurrency limiting | The `Semaphore` pattern from `schedule_executors.rs` *(landed in P2 as one process-wide semaphore; `subagent_permits(provider)` expresses each provider's limit as a permit count)* |

The genuinely new code is the domain types, the port, the adapter translation layer, and one MCP
extension.

### 3.4 On-device reality: concurrency 1, and that is fine

One GPU, one resident model. Two subagents on a Jetson do not run in parallel; they interleave
badly, thrash the KV cache, and finish later than if they had run one after another.

So: **default concurrency 1 for anything that runs on this device; parallelism only for a provider
that runs somewhere else.**

**The predicate is `model_class::runs_on_this_device`, not `context_governor::is_local_provider`.**
The first covers `local`, `gguf`, `ollama` and `llamafile`; the second covers only `local` and
`gguf`. Ollama and llamafile speak HTTP, but on a GIAP pond they speak it to `127.0.0.1` — the same
GPU the parent's next turn needs. PAI-4 P2 already had to fix exactly this mistake once. A test
pinning concurrency 1 for `"local"` alone is a vacuity control pinning the wrong number and would
stay green straight through the regression. `max_concurrent_subagents` in the P1 domain is written
against the wider predicate and its test iterates `ON_DEVICE_PROVIDERS` so a provider added later
is covered without anyone remembering the file.

**A deny-set is only half the predicate, and the repair for that landed 2026-08-11.**
`runs_on_this_device` is membership of four names, so its NEGATION is "not known to run here" —
which is not the same claim as "runs somewhere else", and P7/P8 were written as though it were.
`chat_provider` is a flat settings row with no allow-list on the write path; `mock` is our own
in-process provider; goose ships `lmstudio`, `llama_swap` and `omlx` declarative providers that
serve from localhost. Every one of them answered "not on this device" and so got parallelism, a
per-role model and a background run. `model_class::provider_locality` now answers `OnDevice`,
`Hosted` or `Unknown` against two named lists, the gates match on it exhaustively, and `Unknown`
takes the on-device answer. `HOSTED_PROVIDERS` is a positive claim: a genuinely hosted provider
missing from it loses a background run and a role model, which is the direction this programme
requires.

**Concurrency 1 is necessary and not sufficient.** `goose-local-inference`'s `LoadedModel` holds
`session: Option<SessionKv>` — exactly ONE retained KV prefix per loaded model slot, process-wide,
behind one async mutex. An interleaved subagent turn does not merely queue behind the parent: it
**overwrites the parent's prefix**, so the parent pays a full re-prefill (the measured 3.7 s on the
Orin) on its next turn. The semaphore therefore has to serialise subagent turns against **parent**
turns, not just against each other. Note also that a synchronous subagent runs inside a held
`sse_semaphore` permit, and that semaphore is `Semaphore::new(4)`: four concurrent delegating turns
exhaust the entire interactive chat pool.

The value of delegation on-device is therefore **not** wall-clock speed. It is:

- **Context isolation.** A subagent burns its own window on a messy sub-problem — twelve tool calls,
  a long document — and returns three sentences. The parent's context grows by three sentences
  instead of twelve tool results. On a 4K window that is the difference between finishing the task
  and being compacted mid-conversation.
- **Tool isolation.** A role gets four tools instead of fifty-nine, which is measurably better tool
  selection on small models and a smaller prompt (Phase D measured 6,539 → 2,386 prompt tokens for
  59 → 17 tools).
- **Failure isolation.** A subagent that loops burns its own turn budget and returns an error.

Saying this plainly matters because "multi-agent" invites an assumption of parallelism, and shipping
serialised parallelism would be complexity with no payoff.

**AS LANDED (P8, 2026-08-11): this section is now enforced at the tool surface, not only in the
scheduler.** A `delegate` call carrying `background: true` is REFUSED outright on any provider
`max_concurrent_subagents` limits to 1, with a sentence that names the provider and tells the model
to ask again without it. The alternative — quietly running it in the foreground — was rejected for
the reason this section gives: the caller was told its turn would return immediately, and it would
not. The same reasoning refuses a per-role model on the same providers (P7): one model slot, one
retained prefix, and the parent pays for anything that displaces either.

### 3.5 Budget: a subagent is a second claim on one window

Each spawn takes `context_fraction` of the parent's budget via the governor. The parent's own
history budget shrinks while a child is live. Without this, two agents each believe they own the
whole 4K window and the second one's first turn overruns.

**Where that reservation goes, verified 2026-08-07.** The seam is not `ContextGovernor`, which is
stateless and only answers "how big is this model's window". It is
`CompactionProfile::for_windows`, whose single live producer is `GooseAdapter::profile_for`, and
the only consumer that trims is `turn_trimmer::trim_history` — one call site outside its own file.
The reservation must shrink **`history_token_budget` only**. Scaling the resolved window before
`ContextGovernor::prompt_window` would shrink the preamble clamp too and move the KV prefix, which
costs the 3.7 s re-prefill. Good news found while checking: `memory_token_budget` feeds
`memory_block_for_user_msg`, which rides the **user** message, so shrinking it does not move the
prefix either. `turn_profile()` is stateless and per-turn, so P4 needs new adapter-owned state that
it reads; keep the live-child ledger in the orchestrator and pass a reservation in, rather than
putting a mutable budget on `CompactionProfile`.

Nested delegation is capped at depth 1 in v1. A subagent may not spawn. P1 makes that structural
rather than a counter: `DelegationDepth` has no public constructor from a number, no `Default` and
no `Deserialize`, and the only public path to a deeper authority is
`DelegationAuthority::delegate` → `TaskSpec::child_authority`, which refuses at
`MAX_DELEGATION_DEPTH`. A child is handed its authority; it never builds one, so there is no value
for it to get wrong. The strongest enforcement remains structural in the adapter too: if
`giap-orchestrator` is simply not in a child's `TaskConfig.extensions`, the child physically has no
`delegate` tool.

### 3.6 The tool surface

One new extension, `giap-orchestrator`, behind `ext_orchestrator_enabled` (making 16 registered
extensions — there are **15** today, not 14; `giap-toolkit` registers via the `TOOLKIT_EXTENSION`
const, so grepping for `"giap-*"` string literals undercounts it):

- `list_roles` — what personas exist, and what each is for.
- `delegate` — run a role against instructions. Synchronous by default; `background: true` only when
  the provider supports concurrency.
- `check_task` — poll a background task.

Three tools, not five, and the descriptions are written for a home assistant. The extension joins
the core group set only when enabled, so it costs nothing in the common case.

**AS LANDED there are TWO, not three (P5 2026-08-10, P8 2026-08-11).** `delegate` and `check_task`
exist; **`list_roles` was never built and no phase owns it.** That is worth saying plainly rather
than leaving the list to read as an inventory: `delegate` requires the caller to name a role
exactly, and nothing in the tool surface tells a model which roles a pond has — so today the model
learns a role's name from the user, or it does not delegate at all. Whoever builds it should note
that it is the one tool here that is a pure read of `agent_recipes` and could plausibly live on
`giap-toolkit` instead.

`check_task` carries its own boundary and it is not the same one as `delegate`'s: `Orchestrator::poll`
is keyed by task id alone and an id is a UUID the model was told, so `authorise_task` refuses a run
whose `parent_session_id` is not the caller's — otherwise one household member's conversation could
read back another's result by quoting an id. A foreign task and a nonexistent one get the same words
and different traces, exactly as the four unauthorised inputs do.

`AGENTS.md`'s "62 tools when every toggle is on" became **63** when `check_task` landed. The
registration cross-check (`registration_matches_the_catalog.rs`) ties the EXTENSION count, not the
tool count, so nothing fails; the sentence is stale and `AGENTS.md` is outside this change's
footprint.

Two things this extension must NOT do. It must not appear in `dispatcher.rs` or in
`DIRECT_DISPATCH_ALLOWLIST`: that path runs a tool with no chat turn and therefore no caller, and
delegation decides and executes. And if a subagent can ever run under a `Guest` parent,
`giap-orchestrator` belongs in `groups_denied_to_guests()` — delegation is a way to reach a tool
you were denied.

### 3.7 Streaming — LANDED (P6, 2026-08-10)

`AgentStreamEvent::SubagentProgress { task_id, role, status, detail }`, so the desktop can render
the tree instead of a spinner. Subagent output is **not** persisted into the parent's
`session_messages` — only its final result, as a tool result, which is what keeps the context
isolation real.

As landed, `status` is a closed `SubagentStatus` enum (`queued` / `running` / `tool` / `completed` /
`cancelled` / `turn_budget_exhausted` / `failed`) and `detail` carries a tool NAME or a
GIAP-authored failure reason — never the child's prose, its reasoning, or a tool call's arguments.
The transport is in the block below; the correction that follows is what unblocked the phase and is
worth reading before anything else in this section.

#### CORRECTION (2026-08-10): BOTH channels this section named are unreachable, and P6 needs neither

The paragraph that stood here told an implementer to use two things that do not exist on GIAP's
path, and it is why every attempt at P6 stalled at "let me look at the code seams". It said: *"P6
needs both channels: `on_message` (a synchronous `Fn(&Message)` called inline in the drain loop) for
lifecycle and turn counting, `notification_tx` for the tool name."*

`on_message`, `OnMessageCallback` and `notification_tx` are all fields of **`SubagentRunParams`**,
the parameter struct of `run_subagent_task` — and `goose/crates/goose/src/agents/mod.rs` declares
`pub(crate) mod subagent_handler;`, re-exporting only `SUBAGENT_TOOL_REQUEST_TYPE` and `TaskConfig`.
**P2 decided not to call `run_subagent_task` and could not have if it wanted to.** This section was
written before that decision and never revisited, which is the third time PAI-6 has specified a
phase against code that is not reachable (`subagent_system.md` deleted, `ModelRouter` never
compiled).

Worse than unreachable: **`notification_tx` is a name that also exists in GIAP**, in `main.rs`, as
the broadcast behind `GET /notifications/stream`. It is an unrelated channel. An implementer who
greps the symbol finds a live one and wires P6 to the wrong thing.

**Owning the child loop makes both unnecessary, and gives P6 strictly more than either offered.**
`GooseAdapter::run_child_agent`'s drain loop already receives every `AgentEvent::Message` inline —
that IS `on_message`, with ownership rather than a borrow — and `msg.content` carries every
`MessageContent`, so `ToolRequest` is in hand without a second channel. Two things follow, and both
are improvements rather than compromises:

- **PAI-2 minimisation gets stronger, not weaker.** The old design had the arguments cross a channel
  verbatim — on this pond they can carry household memory queries and device state — and had P6 drop
  them on receipt. Reading `ToolRequest` in the loop means the tool NAME is taken and the arguments
  are never put on a channel at all. Do not reintroduce a channel that carries them.
- **PAI-5's reasoning gate still has to be re-applied here**, and this is the one part of the
  original paragraph that survives intact. `MessageContent::Thinking` is right there in `msg.content`.
  PAI-5 P1 gates once at the `GooseAdapter` producer and the child path does not go through it, so
  without a gate a subagent's reasoning reaches the consumer on an install with `show_thinking` OFF.
  Note the existing drain deliberately uses `as_concat_text()`, which returns `None` for `Thinking`
  and is load-bearing for exactly this reason — a P6 that reads `msg.content` directly loses that
  protection and must replace it rather than inherit it.

**What was genuinely still open is the transport, and it was the real design question.**
`run_child_agent` executes *inside* the parent's turn, underneath the `delegate` tool call, while the
parent's `async_stream::stream!` is parked on `goose_stream.next().await`. A frame produced in the
child loop therefore has no path to the parent's stream without a side channel that the parent's
loop also selects on. That was P6's actual work: pick the channel, key it so a frame reaches the right
parent and only that parent, and make the parent's drain `select!` over both without starving either.
Do not spend time on `on_message`/`notification_tx`; they are a dead end this document sent people
down.

#### THE TRANSPORT, AS LANDED (P6, 2026-08-10)

`ProgressBus` in `orchestrator.rs`: an unbounded `mpsc` per live turn, in a process-wide map keyed by
the parent's **GIAP session id**, with the parent's drain running `next_parent_step` — a `biased`
`select!` with the progress receiver first and `goose_stream.next()` second.

- **Unbounded**, because the sender is the child loop running underneath the parent's own poll: a
  bounded channel that filled would deadlock the future that drains it. The volume is a frame per
  lifecycle transition plus one per tool call, against a role capped at twelve turns.
- **Keyed by session and handed to ONE subscriber**, not broadcast-and-filter. A filter is a thing
  that can be got wrong, and getting it wrong shows one member's delegation inside another's chat.
  Routing at the map makes the failure a lost frame instead of a misdelivered one. The residual
  coarseness — two concurrent turns of one session, newest subscriber wins — is the same one
  `DeviceLedger` and `parent_turn_token` already have and cannot cross a profile boundary.
- **The bias is progress-first, and it cannot delay an engine event.** A synchronous delegation is
  produced underneath the parent's own `delegate` tool call, and goose awaits `ToolCallResult::result`
  inline — so for the whole of a child's run the engine branch is Pending and there is nothing for
  the bias to get in front of. What it buys is that a frame is delivered as soon as the engine is not
  ready, rather than queueing behind however many events it has to hand. Both directions are pinned
  by tests, one against an engine that parks mid-poll (the real shape) and one against a hot engine
  (P8's shape, pinned early).
- **Cancel-safety** is what the loop rests on: losing the race drops a `Next` future, not the
  stream, and an `async_stream` generator's state lives in the stream itself.

**PAI-5's reasoning gate is re-applied structurally, not by a check.** `child_tool_names` is the only
thing that reads a child's `msg.content`, and it returns a list of tool NAMES — so there is no value
it can return that carries reasoning, answer text, or a call's arguments. A source tripwire fails if
anything else in the drain reads `msg.content` again.

**Deliberately deferred: a child's reasoning is never surfaced, even with `show_thinking` on.** The
frames carry no child-authored prose at all. Surfacing it would need the parent's per-turn
`show_thinking && !voice_mode` value inside the child loop, which does not have it, and the child's
own `PromptState` sets `thinking_enabled: false` — so the feature would ship with a gate to maintain
and almost no input to show. If it is ever wanted, the honest shape is a flag declared by the
SUBSCRIBER (the parent's turn knows the value, including voice) and enforced at `ProgressBus::publish`,
not a producer-side check.

Also compiler-forced: a new `AgentStreamEvent` variant breaks **three** exhaustive matches —
`routes.rs :: TurnAccumulator::absorb`, `chat.rs`'s voice/workflow consumer, and `main.rs`'s CLI
renderer — but **not** `pond-agent/src/agent.rs` or `delegation.rs`, both of which use `_ => {}`,
and `pond-agent` is `cargo check`-only in CI.

**This paragraph said FOUR matches and "P6 writes its arm twice" until 2026-08-10, and that is now
wrong**: PAI-5 P7's unification landed, so `chat_stream` and `agent_chat_stream` both fold their
events through one `absorb`, and `stream_handler_parity.rs` fails if a second match appears in
either. P6 wrote its arm once. `SubagentProgress` is on that guard's distinctive-variant list, and
a partial `match … _ => {}` bolted beside the fold in `agent_chat_stream` fails it by name — the
mutation was run.

---

## 4. Phases

- **P1 — LANDED 2026-08-07.** Domain: `AgentRole`, `TaskSpec`, `TaskRun`, `Orchestrator` port.
  Roles stored in the existing **`agent_recipes`** table; no new persistence, no migration.

  I built this to make the two invariants that matter structural rather than checked. `TaskRequest`
  — the part a model supplies — has no scope, no tool and no depth field, and it is
  `deny_unknown_fields`, so a `delegate` call that tries to carry one is a parse error rather than
  a silently-dropped field. The child's scope comes from `RolePersonalData`, an enum whose codomain
  is "the parent's scope" and "Guest" and **contains no widening value at all**; `DelegationDepth`
  has no `Deserialize`, no `Default` and no public constructor from a number, so a child cannot
  forge one. `TaskSpec` has private fields and exactly one producer.

  **Respecified against the phase text, and why.** The design's `AgentRole` carried
  `profile_scope: ProfileScope` and `model_role: ModelRole`. I dropped both. An absolute scope on a
  stored role is the widening hazard the whole workstream is about — a role saying `household`
  would widen a `Guest` parent — and which member is speaking is a per-turn fact a stored config
  cannot know; the only role-level restriction that is meaningful *and* monotone is "drop to
  Guest", which is what `RolePersonalData::Deny` is. `ModelRole` does not exist and `ModelRouter`
  never compiled, so defining a type here would have been P7 done badly and early. I also dropped
  `spawn`'s second `parent: &SessionRef` argument: the spec already carries the parent session id
  and two sources of truth for an authorisation input is exactly how one gets widened.

  **What I rejected.** Refactoring `DelegatingAgent` into the port — it forwards the request
  unchanged, which is invariants 1 and 4 inverted, and it selects on message text, which is the
  keyword classification the working agreement forbids. A no-op adapter to make the port look
  wired — this programme already has three correct-but-unreachable mechanisms and each cost a
  later round more than the gap would have. Adding a `roles` table: the role fields ride one
  GIAP-owned YAML key, `giap_role`, inside an existing `agent_recipes` row, and both readers of
  that YAML (`RecipePrompt`, Goose's `Recipe`) ignore unknown keys, so the recipe still runs as an
  ordinary routine.

  **Still not done, and owed by later phases.** Nothing calls any of it. The
  `DelegationAuthority::root(...)` call at the API edge, which is what makes the parent's real
  post-selection tool set the ceiling, is P3's. A role stored this way is also visible in the Hub's
  Routines list, because `hubDataStore.ts` maps every recipe to a routine card — a deliberate
  consequence of "no new persistence", not an oversight, and P5 should decide whether to filter it.
  `max_concurrent_subagents` has no caller until P8.
- **P2 — LANDED 2026-08-07.** `GooseOrchestrator` in `pond-adapters-goose`, implementing P1's
  `Orchestrator` port. `crates/pond-adapters-goose/src/orchestrator.rs` plus a
  `driving a child agent` block in `goose_agent.rs`.

  **The visibility question, answered: I own the child loop; there is no sixth fork patch.**
  `run_subagent_task` is `pub(crate)` and unreachable, which the phase text did not know. I could
  have made it reachable with two words on the fork, and I did not, because it would have bought a
  function whose child cannot be given a GIAP prompt: `build_subagent_prompt` renders Goose's own
  `subagent_system.md` unconditionally and calls `override_system_prompt` on an `Agent` it
  constructs internally and never returns. A patched-visibility P2 would ship a subagent that
  introduces itself as "a specialized subagent within the goose AI framework, created by AAIF",
  and the provider shim would not catch it — `GOOSE_DEFAULT_MARKER` is not in that template. The
  owned loop is about 120 lines and cost less than the patch would have, once the CI fork-branch
  bump and the rebase burden are counted. **The patch set stays at five**; `AGENTS.md` said "two"
  and now says five, because I counted the rows while deciding this.

  **Respecified against the phase text, and why.** The bullet said "render `subagent_system.md`".
  That file was deleted on 2026-08-06 with the whole of `giap_prompts.rs`. The child's system
  prompt is now built through the LIVE path — `build_prompt_partition`, the same call the parent's
  turn makes — and a GIAP-authored delegation envelope is appended to it: the role, the turn
  budget, "you cannot delegate", and the exact tool names the child holds. Nothing is translated
  into a `Recipe` at all: `Recipe` exists only to be consumed by `run_subagent_task`, which I do
  not call, and `recipe.extensions` is never read by that loop anyway. `TaskConfig` likewise —
  five of its six fields are inert on the path that would have used it. What survives from the
  bullet is every one of its imperatives: `Some(max_turns)` always, `GooseMode::Auto` always
  (any approval-requiring mode deadlocks on the child's `confirmation_rx`), the last assistant
  message only, and an extension list built explicitly rather than inherited.

  **What I rejected.** Reusing `GooseAdapter::builtin_extension_config` to build the child's
  extension list — it passes `available_tools: vec![]`, which Goose reads as *all tools of that
  extension*, so the one-line reuse is a scope-widening default. `child_extensions` populates the
  allowlist with real unprefixed names (which is what `dispatch_tool_call` matches) and **drops**
  an extension whose granted list comes out empty rather than emitting one. I also rejected — and
  then deleted after writing them — two redundant checks: an extension-level
  `spec.tool_groups().contains` and a second `GOOSE_STRIPPED_BUILTINS` skip inside the parent
  loop. Both were unreachable behind `TaskSpec::grants_tool`, and I only found out because
  deleting them left every test green. An unreachable guard with a test that claims to cover it is
  worse than no guard.

  **Invariant 2 now has one list instead of three.** `goose_agent.rs` carried two hardcoded copies
  of the ten stripped builtins — `strip_list`, which enforces, and the `is_builtin` prompt filter,
  which does not — and this phase would have added a third. All three read
  `orchestrator.rs :: GOOSE_STRIPPED_BUILTINS`, and a canary fails if `goose_agent.rs` ever spells
  one of those names again. Calling the child loop directly does not bypass the strip, because
  `Agent::with_config` builds an `ExtensionManager` with no extensions and nothing auto-loads
  defaults into it — so the child is audited *after* the run as well, and a child that somehow
  ended up holding `summon` has its result discarded rather than returned.

  **Invariant 3 is a permit, not a convention.** One process-wide `Semaphore`; `spawn` acquires
  `subagent_permits(provider)` of it before touching the engine, so there is no start path that
  does not queue. The predicate is `max_concurrent_subagents`, which asks `provider_locality` —
  `local`, `gguf`, `ollama`, `llamafile` run here, and so does any name in neither list — and the
  test iterates `ON_DEVICE_PROVIDERS` rather than naming two of them, with a second test over
  `mock`, `lmstudio`, `pond-spark` and the empty string for the unknown half.

  **Still not done, and owed by later phases.** Nothing calls `spawn`: the first caller is P5's
  `delegate` tool, and P3 owes the `DelegationAuthority::root(...)` at the edge and the
  `engine_session_map` row that lets the draft gate resolve a child at all. **The semaphore does
  not cover the parent's turns**, which section 3.4 says it must: a subagent turn still overwrites
  the single retained KV prefix that `LoadedModel.session` holds, so the parent pays a re-prefill
  on its next turn. Closing that means the parent's turn path taking the same permit, which
  belongs with P4's reservation work. `context_fraction` is carried on the spec and read by
  nobody — also P4. And `run_child_agent` itself has no test: it needs a real provider and a real
  Goose session store, so everything that DECIDES was split out into pure functions and the
  registry, and those are driven through the real `spawn` against a fake engine. The engine drive
  is owed a live run.
- **P3 — LANDED 2026-08-09.** Scope inheritance. The turn's real authority published at the edge,
  tool groups narrowed by the role *and* by the child's own scope, and the draft gate answered by
  withholding rather than by checking.

  **The draft half, respecified — read this before assuming it was skipped.** The bullet said
  "draft gate enforced inside subagents", and called it a mapping task. It cannot be either.
  `engine_session_map.session_id` is the PRIMARY KEY, so pointing a child's engine session at the
  parent's row would **overwrite the parent's own pairing** and lose its conversation; N children
  to one parent needs a second table, i.e. the migration and persistence this workstream keeps
  refusing. And even a working mapping would be *wrong*: a role with `personal_data: deny`
  produces a `Guest` child, and resolving that child through the parent's session hands it the
  parent's scope straight back — the gate would then permit exactly what the role withheld. There
  is no approval path to fall back on either, because a subagent is forced to `GooseMode::Auto`
  (any approval-requiring mode hangs forever on the child's `confirmation_rx`). So the
  enforcement is `tool_group.rs :: groups_denied_to_subagents()`: **`giap-draft` is not something
  a subagent can hold.** That is the same move PAI-1 P5 made for guests, for the reason the
  checklist already records — *when the thing you want to check has no identity, move the check to
  the layer that hands it out.* Four more groups are on that list for mechanisms rather than
  taste: `giap-toolkit`, because `enable_tool_group` widens an allow-set keyed by the
  process-global `current_session_id()`, so a child holding it could widen its own narrowing or
  its parent's; `giap-device-control` and `giap-system`, which actuate, write and execute with no
  approval path left once draft is gone; and `giap-schedule`, which commits future work carrying
  the household's authority long after the delegation has ended. The cost is stated in the code
  and accepted: **a subagent has no clock**, because `get_current_time` sits in `giap-system` next
  to `write_file`. A role that needs the date gets it in its instructions.

  **The narrowing that was missing, and it was the sharpest defect in the phase.** P1 computed the
  child's tool set from the intersection alone. A `Household` parent running a role with
  `personal_data: deny` therefore produced a child whose scope said `Guest` while it still held
  `giap-memory` — whose MCP tools carry no session at all and read the household's memory
  regardless of what any scope says. `narrow_child_groups` now subtracts `groups_denied_to_guests`
  keyed on the **child's** scope, not the parent's. A scope that says Guest while the tools say
  Household is not a narrowing, it is a laundering route.

  **The edge, and the input trap.** `DelegationAuthority::for_turn` takes tool NAMES, and
  `chat_stream` hands it the same `allowed_tools` binding it publishes to `ShimControls` — after
  section 6d's guest subtraction, not before. Taking group names would have let a second list
  drift from the first; taking the catalog would have made every downstream intersection a no-op,
  which is precisely how PAI-1 P5 shipped inert. `the_turn_authority_is_built_from_the_published_allow_set`
  fails if the publish is moved above the subtraction, if it stops using that binding, or if the
  scope stops coming from `turn_scope`. That guard reads source because the construction is inside
  a function that needs a live engine; it is a tripwire on the INPUT, which is what PAI-5 P1
  lacked when it gated on `show_thinking && !voice` with the other input hardcoded false.

  **A silent P2 defect this phase found and fixed.** A child's system prompt is the parent's static
  prefix plus GIAP's delegation envelope, so `enforce_system`'s `incoming.starts_with(prefix)`
  matched and **the shim rebuilt the envelope away on every provider call**, splicing in the global
  extension appendix in its place. The envelope is where a child is told its turn budget, that it
  cannot delegate, and the exact tool names it holds. It failed silently in both directions: the
  rebuild "succeeded", so `system_appendix_dropped` never fired. `SessionControls::set_system_override`
  is the fix, and the two tests that pin it drive the real `Provider::stream` — every existing shim
  test calls `enforce_system` directly, which is exactly why nothing saw this.

  **Invariant 5's other half is closed, and not by the method that was written for it.** P2 left
  `cancel_children_of` with no caller because the parent turn's token was a stack local. The token
  is now published beside the authority, and `spawn` derives the child's token from the parent's
  with `child_token()` — so a voice interrupt, a dropped stream or a client hanging up reaches the
  child through the `DropGuard` that already exists, with nothing having to remember to call
  anything. `cancel_children_of` remains for the explicit cases (a deleted session, a shutdown).

  **What I rejected.** A second table mapping child engine sessions to parents — see above; it
  would have been persistence for a resolution that is wrong even when it works. Threading a
  `ProfileScope` into the MCP servers per call so a subagent's `giap-memory` reads could be
  scoped — the only mechanism there is the process-global `current_session_id()` that PAI-1
  already refused to build on, and it would trade a withheld group for a misattribution bug.
  Refusing a delegation outright when its role names a denied group: filtering is what
  `subtract_guest_denied_tools` does, the envelope tells the child its real list, and a role that
  names `giap-draft` alongside four useful groups should still run.

  **Still not done.** Nothing calls `spawn` — P5's `delegate` tool is the first caller, and it is
  what turns the registry from a published fact into an answered question. `run_child_agent` still
  has no test for the same reason P2 recorded: it needs a real provider and a real Goose session
  store, so the shim publication inside it is guarded by a source canary on the ORDER of the calls
  rather than by observing the provider. And a subagent's `giap-memory` reads are still unscoped
  at the MCP server, exactly as they are for an Owner's own turn — PAI-1's open gap, not one this
  phase widened.
- **P4 — LANDED 2026-08-09.** (Since 2026-09-11 the history shrink reaches only `plan_replay`, because
  goose trims the live conversation; the device hold is unaffected.) Budget: `context_fraction` reaches `CompactionProfile`, the parent's
  history budget shrinks while a child is live, and the parent's own turns now take the same
  concurrency permit a child does.

  **The reservation touches one field, and that is the whole design.**
  `CompactionProfile::with_history_reserved` moves `history_token_budget` and nothing else. The
  implementation that suggests itself — re-derive the profile from a scaled window — compiles,
  reads well and shrinks every preamble allowance with it, so the system prompt is rebuilt at a
  smaller size and the KV prefix MOVES: a full re-prefill charged to the parent's next turn, to
  save tokens on a working set the trimmer was about to cut anyway. Delegating would then cost
  exactly the thing delegation is justified by. `a_reservation_takes_working_set_and_never_preamble`
  asserts every field by hand and
  `a_live_child_shrinks_the_parents_history_and_leaves_its_prefix_alone` asserts the adapter applies
  it that way rather than by scaling its resolved window.

  **Respecified against the phase text: the reservation comes out of the CLAMPED budget, not the
  declared one.** Section 7's assertion is only true that way. At an 8,192 window the profile
  declares 4,000 history tokens while `usable_history_tokens` allows 3,668, so taking 30% of the
  declared number leaves the parent 2,800 — the clamp then cuts it back to 2,800 anyway, and 2,800
  plus the child's 1,200 plus the preamble is 7,500 against a 7,168-token usable prompt. Two agents
  each believing they own the window is the failure this exists to prevent, and subtracting from
  the declared allowance reproduces it. Reserving out of `min(declared, usable)` makes the section 7
  disjunction hold exactly: the sum fits, or the parent is on `MIN_HISTORY_TOKENS` and the declared
  sum overshoots by exactly that floor.
  `a_parents_budget_and_its_childs_reservation_fit_the_window_or_hit_the_floor` sweeps eight windows
  by seven fractions and fails if a case reaches neither branch;
  `a_child_that_takes_the_whole_budget_leaves_the_parent_exactly_on_the_floor` is the floor case, and
  it asserts that the unconditional form of the assertion would be FALSE there — so a future author
  cannot quietly simplify it.

  **`effective_history_budget` is new, and it exists because the phase had nothing to assert on.**
  The trimmed budget was a local inside a 200-line function, and its only downstream observable is
  how many turns got dropped — which moves for four other reasons. `trim_history` now calls it and
  nothing else recomputes the clamp.

  **Invariant 3's other half, which P2 left open and named.** Every on-device parent turn now takes
  `parent_turn_permits` from the same process-wide semaphore, held for the whole turn by a guard the
  stream drops. The predicate is `max_concurrent_subagents`, so ollama and llamafile — HTTP to
  `127.0.0.1`, same GPU — are covered, and a hosted provider takes nothing at all, so four hosted
  conversations still run four abreast. The cost is stated rather than hidden: two concurrent
  on-device chat turns are now serialised against each other where they used to interleave between
  provider calls. That is a real behaviour change for every install, and it is the right one — the
  engine already serialises the calls themselves behind one mutex, and interleaving only ever bought
  each turn a re-prefill of the other's prefix.

  **The deadlock this could have shipped, and did not.** A synchronous delegation runs INSIDE its
  parent's turn, and that turn now holds every permit on this device. A child that queued for them
  would wait on a parent that is waiting on the child, forever, while holding one of `main.rs`'s
  four `sse_semaphore` permits — so four delegating turns would take the interactive chat pool down
  until the process restarted, answering every later stream 503. Nothing calls `spawn` until P5, so
  it would not have surfaced in this phase at all. A child whose own parent's turn holds the device
  therefore INHERITS it instead of acquiring, which is not a relaxation: the parent cannot reply
  while its child runs. `a_child_runs_under_its_parents_device_claim_rather_than_deadlocking_behind_it`
  fails by timeout without it, and
  `a_child_of_another_session_waits_for_the_turn_that_holds_the_device` is its vacuity control —
  inheritance is keyed on the child's own parent, not granted to everybody.

  **And the first version of that pass BROKE invariant 3 for siblings — repaired 2026-08-09.**
  Inheriting was written as `needed = 0`, and `acquire_many_owned(0)` never blocks. A device hold
  belongs to the SESSION, not to one child, so every child of the delegating turn inherited it:
  three delegations issued in one parent turn ran three abreast on the one GPU, which is precisely
  what the invariant forbids and precisely what one retained KV prefix cannot survive. Nothing saw
  it, because `only_one_on_device_child_runs_at_a_time` does not hold a parent claim — the state P4
  made normal — and because the phase's own mutation of `inherited = true` produced `left: 3` in
  that test and was read as a test-only artefact. The repair is that a parent's hold carries its own
  `Semaphore` of ONE for its children to share: a child never queues behind its own parent, and
  siblings serialise exactly as any two on-device children do.
  `siblings_of_one_delegating_turn_still_run_one_at_a_time` is the permanent test and reports
  `left: 3` against the version that shipped. The generalisable lesson is that **a fix for a
  deadlock is a concurrency change and needs its own concurrency test**, and the specific one is
  that a `bool` was the wrong answer to "may this child skip the queue" — the question is *which*
  queue, so the ledger returns the queue.

  **The `sse_semaphore` consequence is real and is NOT closed here.** Even with inheritance, a
  delegating turn holds one of four interactive permits for as long as its child runs, so four
  concurrent delegations still leave nothing for a fifth stream. `Semaphore::new(4)` lives in
  `main.rs`, outside this phase's footprint, and the fix is not a bigger number — it is that a
  delegation which may run for minutes does not belong on the interactive pool. **P5 owns this
  decision**, because P5 is what makes a delegating turn reachable: either the `delegate` tool
  returns quickly and delegation becomes P8's background path, or the SSE pool gains a separate
  allowance for turns that are waiting on a child rather than on a provider.

  **Depth was verified, not reimplemented.** `MAX_DELEGATION_DEPTH` is 1;
  `DelegationDepth::deeper` is private and refuses at the cap; the type has no `Deserialize`, no
  `Default` and no public constructor from a number, so its only public path remains
  `delegate` → `child_authority`. `nothing_in_this_adapter_can_construct_a_delegation_depth` reads
  `orchestrator.rs` and `goose_agent.rs` with comments stripped and fails if this crate ever names
  the constructor, and it reads the cap from the domain rather than restating it.

  **What I rejected.** A mutable budget field on `CompactionProfile` — section 3.5 warned against it
  and it would have made a per-turn value into shared state. Passing the ledger as a constructor
  argument — same argument as the semaphore: `DeviceLedger::default()` at a call site looks exactly
  like correct wiring and silently answers "no child is live". Reading the reservation at the three
  budget call sites instead of once inside `turn_profile` — PAI-3 P5 exists because two of four
  sites forgot the prompt-side clamp, and this would have been the same defect one phase later.
  Giving the child its own trimmed history budget: nothing trims a child's conversation, its length
  is bounded by `max_turns`, and inventing a second trim path for it is P8's problem if it is
  anyone's.

  **Five of P4's and P2's guards could not fail, and were repaired in the same change 2026-08-09.**
  Each was found by applying a production-shaped mutation and running the suite, and each was green:

  - `spec.context_fraction()` replaced by the literal `0.3`. Every role fixture in the adapter test
    file stated 0.3 and the only observing assertion said 0.3, so the guard could not tell reading
    the role's fraction from restating the fixture's constant — a role authored at 0.8 would have
    reserved 0.3 forever. `role_with_fraction` now spawns at 0.75 and 0.4, and no single literal
    satisfies both.
  - `claim_device_for_turn` deleted from `chat_stream`. The one production call site of invariant
    3's parent half — the only part of P4 that changes behaviour on every install today — had no
    guard at all, because every device test either calls `parent_turn_permits` as a pure function or
    takes the claim from its own body. That is vacuity shape 2, a gate tested while its input is
    unguarded. `the_live_turn_claims_the_device_before_it_streams_anything` reads `chat_stream`'s
    body, and it checks the BINDING too: `let _ = claim_device_for_turn(..)` drops the permit on the
    same line and reads as correct.
  - `turn_profile` reading the ledger under `goose-{id}`. A GIAP-versus-Goose session id mix-up is a
    mistake this codebase has already made once, and it leaves every reservation reading 0.0 in
    production with the number looking right. `profile_for_session` now takes the ledger as an
    argument, so a test can run it — `a_parents_budget_shrinks_for_its_own_sessions_children_and_for_nobody_elses`
    — and the residue the tripwire owns is one line: which key the live turn passes.
  - The drain loop's per-event reduction. See invariant 4 above: the P2 semantics could be restored
    in full, and every child made to answer nothing, without moving one of the five strings the
    tripwire looked for. `child_stream_step` is the lifted reduction; what the tripwire asserts now
    is the two ARGUMENTS the loop passes and that nothing else in it touches the assembler.
  - The post-run half of the invariant-2 audit. The guard asserted the POSITION of a
    `list_extensions()` call, not that its result reached `loaded_extensions`, so a read whose result
    was discarded passed. The window is now the text between the reply and the returned
    `ChildOutcome`, and the assertion is that the statement extending the audited set is the one
    that reads the child.

  A sixth, latent: the single-producer count read `goose_agent.rs` while its message spoke about
  "this adapter", so a second `CompactionProfile::for_windows` in `orchestrator.rs` escaped it —
  vacuity shape 4, a window too narrow for the natural regression. Both that guard and the
  delegation-depth canary now walk the crate's `src/` directory, so a file added tomorrow is covered
  on the day it is added rather than on the day somebody remembers to list it.

  **Still not done.** A synchronous delegation cannot shrink its OWN turn's trim: `trim_goose_history`
  runs before `Agent::reply`, so by the time a child exists the parent has already budgeted. The
  reservation binds a *concurrent* turn of the same conversation and, when P8 lands, a background
  child that outlives the turn that spawned it. That is worth saying plainly because the phase text
  reads as though it binds the delegating turn itself, and it does not. Section 3.4's measurement —
  parent context growth with delegation versus inline — is still owed and still needs the Orin.

  One guard is still asymmetric and is `pond-core`'s, so it is not repaired here:
  `a_parents_budget_and_its_childs_reservation_fit_the_window_or_hit_the_floor` asserts
  `floored > 0` but never that any case reached the NON-floored branch, so a change that floored
  every case would leave the real inequality unevaluated with the sweep green. Instrumenting it says
  the branch IS reached today — 28 floored of 112 checked, 84 asserted — so the test is sound as it
  stands; what is missing is the control that pins it. The one line is
  `assert!(checked - floored > 50, ...)` in `turn_trimmer.rs`.
- **P5 — LANDED 2026-08-10 (`c0f2bf2a`).** The `giap-orchestrator` extension, the `delegate` tool,
  and `ext_orchestrator_enabled`.

  **This is the phase that made the workstream real.** P1's authority, P2's child loop and P3's
  turn-authority registry were all correct and all inert, because nothing called
  `Orchestrator::spawn`. `crates/pond-mcp-server/src/orchestrator.rs` is the caller. It is handed the
  CALLER's engine session id in `_meta` — stamped by the engine rather than chosen by the model,
  because `inject_session_context_into_extensions` retains away any caller-supplied `agent-session-id`
  and re-inserts its own — resolves it through `TurnAuthorityRegistry::authority_for_engine_session`,
  and refuses when that answers `None`. **Four inputs produce `None`** — a session GIAP never chatted
  in, a subagent's own session, a turn that has ended, and a call carrying no `_meta` at all — and all
  four get the same refusal, deliberately, because a caller that could tell them apart would
  eventually treat one of them as benign.

  The toggle got its own `default_*` fn, as the bullet required. Reusing
  `Settings::default_ext_enabled()` returns `true` and would have shipped delegation on for every
  install, which is the widening default this programme treats as a bug rather than a preference. All
  five persistence pieces are present or the completeness test would have failed the build.

  **The catalog entry is worth more than the line it costs, and the reason is the one the bullet
  gave**: `is_catalog_extension` treats an unknown name as a *user-added* MCP server, which selection
  deliberately never narrows. A builtin the catalog does not carry therefore becomes the one
  extension that can never be selected away, is never subtracted for a guest, and is never withheld
  from a subagent — for `giap-orchestrator` that inverts the entire intent.
  `crates/pond-core/tests/registration_matches_the_catalog.rs` now ties `AGENTS.md`'s count, the
  registration list and `TOOL_GROUPS` to each other and fails if any two disagree. **It lives in
  `pond-core` rather than in the adapter** because CI runs `cargo test -p pond-core` and only
  `cargo check`s `pond-adapters-goose`, and a guard CI never executes is a guard that fails for the
  first time during a release.

  `giap-orchestrator` is on `groups_denied_to_subagents` and `groups_denied_to_guests`. The depth cap
  already refuses a child's delegation, so the first is belt and braces — but the second reason is the
  real one: a tool a 2-4B model can see and cannot use costs turns off a budget of six discovering
  that. `giap-orchestrator__delegate` is on `MUST_NEVER_BE_DIRECTLY_DISPATCHABLE`;
  `DIRECT_DISPATCH_ALLOWLIST` is untouched.

  `AGENTS.md` is 16 now, and its counting recipe is fixed: it said to subtract the import, but the
  import has no open paren so the grep never counted it, and the recipe yielded 14 — the exact wrong
  number this programme had already recorded twice. The instruction now reads "do not subtract
  anything".

  **Recorded rather than hidden.** The desktop toggle has no test. Neither does any other extension
  toggle, so the hole is pre-existing rather than one P5 dug — but the row is not trivial: it renders
  `=== true` where the shared `EXT_TOOLS` map renders `!== false`, precisely so that a key the server
  has not sent yet cannot read ON for the one module that defaults off. And both agents that wrote
  this phase died mid-run on API errors, so the work was validated by the coordinator against the
  gates rather than by its authors.
- **P6 — LANDED 2026-08-10.** `AgentStreamEvent::SubagentProgress { task_id, role, status, detail }`,
  the `ProgressBus` that carries it out of a child's loop, and the tree both desktop chat surfaces
  draw from it. Section 3.7's `THE TRANSPORT, AS LANDED` block holds the design and what was
  rejected; this is what the phase is worth reading for.

  **The correction dated 2026-08-10 above saved the phase and is the reason it exists.** Five
  attempts stalled on `on_message` and `notification_tx`, which are fields of a parameter struct
  behind `pub(crate) mod subagent_handler` — and `notification_tx` is also a live GIAP symbol
  (`main.rs`'s `GET /notifications/stream` broadcast), so grepping it finds a real channel with
  nothing to do with this. Owning the child loop means every `AgentEvent::Message` is already in
  hand.

  **Respecified against the phase text.** The bullet said "streaming"; what a subagent streams is
  the thing invariant 4 forbids reaching the parent. So the frames carry lifecycle plus tool NAMES
  and nothing else: no child prose, no reasoning, no tool arguments (PAI-2 minimisation — on this
  pond they can be a household memory query or device state). The parent still takes back exactly
  one string, the `delegate` tool's result. `absorb_progress_leaves_the_turn_untouched` is the
  consumer-side guard: `TurnAccumulator`'s `full_text` and `tool_results` are what both routes
  PERSIST, and a progress arm that touched either would put a child's activity into
  `session_messages`.

  **What the mutations found.** Reusing the `ToolCall` arm's timing reports the CHILD's tool as the
  parent turn's last tool in `TurnMetrics` — a plausible number about the wrong agent. Swapping the
  select's bias delivers no frames at all against a hot engine, which is the shipped spinner
  restored. Gating the desktop tree on `!streaming` — the shape every other note on a message uses —
  hides it for exactly the minutes it exists to cover.

  **Still owed.** The drain's emit path needs a live provider, so it is covered by an
  argument-and-order source tripwire rather than by a test that runs it; the same limitation the
  P2/P3 tripwires above record. And nothing has yet driven this on the Orin, so the integration
  measurement in section 7 — parent context growth with a delegation versus inline — remains
  untaken.
- **P7 — LANDED 2026-08-11.** Per-role model assignment: `ChildModel` in `pond-core`, an optional
  `model` key in a role's `giap_role` block, and `child_model_config` in the adapter.

  **The collision the bullet named is the phase, and the answer is that the feature is INERT
  wherever the model might be ours to hold.** `ChildModel::resolve` takes the role's request and the
  parent's provider and answers `Assigned` only against a POSITIVE claim that the provider is
  hosted. On-device it answers `RefusedOnDevice`, and for a provider in neither list it answers
  `RefusedUnknownProvider` — each carrying both the model and the provider, so the refusal is a
  value a test can assert on rather than an absence indistinguishable from "the role named nothing"
  — and the child runs on the resident model with the role's tools, budget and persona intact.
  Refusing the whole delegation was the alternative and is worse: a role authored with a model would
  then be unrunnable on the configuration this project is built for, and a role's value is not in
  its model.
  `a_role_model_never_reaches_the_engine_on_a_provider_that_runs_here` quantifies that over
  `ON_DEVICE_PROVIDERS` through the real `build_child_plan`,
  `a_role_model_never_reaches_the_engine_on_a_provider_this_pond_cannot_place` does the same for
  `mock`/`lmstudio`/`pond-spark`/`""`, and
  `a_role_model_is_used_when_the_provider_runs_somewhere_else` is the vacuity control without which
  a resolve that refused everything would pass.

  **The third variant is a repair, dated 2026-08-11, and the shape of the mistake is worth
  keeping.** As landed this was `if runs_on_this_device(provider) { refuse } else { assign }`, so
  every provider name the deny-set had not been taught took the `else` — the permission — including
  our own `mock` and goose's localhost-serving `lmstudio`. A gate whose safe answer is "refuse"
  cannot be written as the negation of a deny-set over an open string domain; it has to ask for
  membership of the set it trusts. See section 3.4.

  **What made it cheap off-device is that there is no router and no second provider.** A child
  already replies through its parent's `Arc<dyn Provider>`; what selects the model is the
  `ModelConfig` on the CHILD's session row, because `Provider::stream` takes it per call and
  `Agent::reply` reads it from the session that `update_provider` wrote. So an assignment is one
  field in a request body — and the parent's cached `(provider, config)` pair is untouched, so a
  delegation cannot move the model out from under the parent's own turn. `context_limit` is CLEARED
  rather than carried: Goose backfills a `None` from the registry entry for the model actually
  named, and a `Some` would budget a different model against the parent's window.

  **On the two dead carriers the bullet asked about: P7 makes NEITHER of them live, and both should
  go.** They are traps, not spare parts.

  - `AgentRequest.model_role` is a per-TURN label — `"chat"` from both stream handlers, `"task"`
    from `schedule_executors.rs` — read once, echoed back in the `Done` frame, and selecting
    nothing. The block headed "GooseMode from model_role" in `goose_agent.rs` is
    `let goose_mode = GooseMode::Auto;` and does not mention it. It cannot become P7's carrier
    either: a delegating turn has exactly one `model_role`, and a subagent's model comes from its
    stored role. Six tests in `pond-api` assert on the `Done` frame's `model_role`, so removing it
    is a `pond-api`/`pond-server`/desktop change and outside this footprint. **The new type is named
    `ChildModel` and not `ModelRole` precisely so nobody reads it as that field's home.**
  - The `think`/`task` rows of `model_role_assignments` are written by exactly one path,
    `POST /models/{category}/{name}/activate`, and `sync_assignments_to_settings` turns them into
    `think_provider` / `think_model` / `task_provider` / `task_model` settings keys — which
    `sqlite_settings.rs :: apply_key` explicitly discards as "Legacy keys — silently ignored". So
    assigning a model to the `think` role today writes two rows nothing reads. P7 does not revive
    them and should not: a two-slot global table cannot express a per-role model, which is the whole
    requirement. Deleting the arms is a `pond-api`/`pond-server` change, outside this footprint.

  **Still owed.** Nothing measures the off-device win, because nothing has authored a role with a
  model. And a role naming a model the provider does not have fails at the provider, as a child
  failure — which is the right direction (loud, isolated to one delegation) but has never been seen.

  **How reachable the `Assigned` branch is, corrected.** The phase record said no UI or API authors
  a role carrying `giap_role.model:`, which reads as "this branch needs a code change to become
  live". It does not. `POST /api/v1/recipes` takes `{name, description, yaml}` and `create_recipe`
  stores the YAML verbatim with no schema check, so any authenticated paired client can author such
  a role today, and on a pond whose `chat_provider` is a name in `HOSTED_PROVIDERS` the branch
  executes. What is true is narrower: no role in this tree carries the key, and no UI control
  writes one.
- **P8 — LANDED 2026-08-11.** Background tasks for providers that do **not** run on this device,
  and `check_task`.

  The bullet was right that there is nothing to reuse — `InferencePool` still has zero
  implementations, `AppState.inference_pool` is still hardcoded `None`, and
  `subagent_execution_tool/` is still serde types — and right that the port did not need to change.
  `Orchestrator::poll` existed for this, `TaskStatus` already had the terminal states, and neither
  gained anything. A background `spawn` returns the `Queued` run it already constructed.

  **The refusal is invariant 3 wearing different clothes, and it is tested against a local provider
  fixture.** `BackgroundAvailability::for_provider` takes its PERMISSION from
  `max_concurrent_subagents` — not a second reading of `runs_on_this_device` — so it cannot drift
  from the number the semaphore enforces; since 2026-08-11 it reads `provider_locality` as well,
  but only to choose which true sentence a refusal says, and the agreement test now spans all three
  localities. A provider in neither list is refused with `RefusedUnknownProvider`, because it was
  PERMITTED before that repair: `mock` and goose's localhost-serving declarative providers all
  answered "not on this device", and there "background" means the household's next reply is
  queueing behind a helper nobody is waiting for. See section 3.4. Where one agent may hold the model at a time, a background child is not background: it
  is the parent's next turn queueing behind it and then paying a re-prefill. `spawn` refuses after
  the provider is known and BEFORE the engine is touched, so a refused call leaves no child engine
  session, no plan and no reservation behind; the test asserts all three, over `ON_DEVICE_PROVIDERS`,
  with the hosted case as the vacuity control. The refusal names the provider and tells the model to
  call `delegate` again without `background`, because a refusal a 2-4B model cannot act on is one it
  retries verbatim.

  **The cancellation decision, stated rather than implied.** PAI-6 P3 derives a child's token from
  the parent TURN's, and `chat_stream`'s `DropGuard` cancels that when the reply ends — so a
  background child that inherited would die with the turn, which makes it a synchronous child with a
  race. **A background run therefore keeps its OWN token, whose explicit owner is the parent
  SESSION**, and its cascade is `Orchestrator::cancel_children_of` (plus `cancel` by task id). Both
  are implemented. `a_background_run_survives_its_turn_and_is_still_cancellable_by_its_session` ends
  the turn exactly as the chat stream does — cancel the token, drop the lease — and asserts both
  halves; `a_synchronous_run_still_dies_with_the_turn_that_asked_for_it` is its vacuity control,
  without which "survives the turn" would pass against an implementation that had stopped deriving
  anything from the turn at all, which is invariant 5's mechanism for every synchronous child.

  **What invariant 5 is now owed, and it is a real gap rather than a rounding error:
  `cancel_children_of` still has NO production caller.** Deleting a session or shutting the server
  down does not stop a background run. That is one call each in `pond-api`'s session-delete handler
  and `pond-server`'s shutdown path, both outside this change's footprint, and it is recorded on the
  method's own doc comment so the absence cannot be read as "nothing needs to call it". Until it
  lands, a background delegation is bounded only by its role's `max_turns` and by the caller
  cancelling it deliberately.

  **What else is bounded and what is not.** A background child holds its `context_fraction` of the
  parent's history budget for as long as it runs, which is what section 3.5 said should happen and
  now finally does with a child that outlives a turn. **That sentence was prose only until
  2026-08-11**: `reserve(parent, if background { 0.0 } else { fraction })` passed all 201 adapter
  tests, because the post-run `reserved_fraction == 0.0` check cannot tell "held then released"
  from "never held" and the inside-the-child fixture was unused on this path. A background `spawn`
  returns with the child still running, so
  `a_background_run_returns_before_the_child_finishes_and_can_be_polled` now reads the claim from
  the CALLER's thread — the one assertion only this case can make — at a fraction (0.6) that no
  other fixture in the file uses. It takes a subagent permit like any other
  child (one of three, off-device). Its progress frames still publish to the parent session's
  `ProgressBus`, so a later turn of the same conversation sees the tree; with no subscriber they are
  dropped, which is P6's stated failure mode.

  **Not done, deliberately.** The bullet proposed the `notification_tx` broadcast plus
  `GET /notifications/stream` as the transport for a finished background task. That is a
  `pond-server`/`pond-api` change, nothing in `pond-desktop/src` subscribes to that stream, and the
  model already has a way to find out — it is told the task id and can call `check_task`. Wiring a
  notification before anything renders it would be a mechanism with no consumer, which is the shape
  this workstream has already shipped three times.

  `giap-orchestrator__check_task` is **not** on `routes.rs :: MUST_NEVER_BE_DIRECTLY_DISPATCHABLE`,
  and should be: it reads a child's answer, which is household data, and the direct-dispatch path
  carries no caller for `authorise_task` to check against — so there is no task id it could safely
  answer. It is deny-by-default today because `DIRECT_DISPATCH_ALLOWLIST` is an allowlist and
  nothing was added to it — but the positive claim belongs beside `delegate`'s. `routes.rs` was
  outside that phase's footprint and outside this repair's; the exact edit is one line after
  `"giap-orchestrator__delegate",` in that array, and it is the only thing this repair leaves
  undone.

  **What `check_task` gained instead, 2026-08-11.** Its own body had never been executed by a test:
  `authorise_task` was covered as a pure function while `run_check` — the only thing that calls it —
  had no coverage at all, so deleting the call left the whole crate green and turned the tool into
  a read of any household member's delegation result by task id. Five tests now drive `run_check`
  through a fake `Orchestrator` whose `poll` is keyed by task id alone, exactly as the real
  registry is: a foreign task, the caller's own (the vacuity control), an unknown id refusing in
  the same words as a foreign one, a caller with no live turn and none carrying `_meta` at all,
  and a guest turn that owns its task and still may not read it — that last being what stops
  `authorise(...)` being replaced by a bare `ok_or`, since the missing-turn case refuses
  identically either way.

---

## 5. Invariants

1. A subagent's scope is a subset of its parent's. Never wider — not tools, not profile, not
   network. *(P1: structural for the profile axis — `RolePersonalData` has no widening value and
   `TaskRequest` cannot name a scope. Runtime intersection for the tool axis, with `grants_tool`
   denying an empty set and an unknown prefix.)* *(P3 makes the ceiling real and couples the two
   axes: `DelegationAuthority::for_turn` builds the parent's set from the turn's actual
   post-selection, post-guest-subtraction allow-set — the same binding published to `ShimControls`
   — and `narrow_child_groups` then subtracts what the CHILD's scope denies, so a role that drops
   a Household parent's child to `Guest` cannot leave it holding `giap-memory`. The child gets the
   boundary twice: `ExtensionConfig::available_tools`, checked inside `dispatch_tool_call`, and
   its own `ShimControls` entry, published before its first provider call so the tool is never
   even listed to it.)* *(The **network** axis is named in the invariant and enforced by nothing
   in PAI-6. It holds today by accident of PAI-2: `NETWORK_MODE` is a process-global
   `static RwLock<NetworkMode>` in `shared/services/egress.rs`, so an in-process child cannot hold
   a mode its parent does not. That accident expires the day a child runs out of process or
   network mode becomes per-session, at which point the axis needs a field on `TaskSpec` like the
   other two. Recorded in the `orchestration.rs` module doc, because there is nothing yet to
   test and therefore nothing that will notice on its own.)*
2. Goose's `summon`, `orchestrator` and `todo` stay stripped. **Building orchestration does not
   mean un-stripping them.** The hazard is not that a direct child-loop call bypasses the strip —
   a fresh `Agent` has no extensions to strip — it is
   `EnabledExtensionsState::extensions_or_default`, whose fallback re-arms every `default_enabled`
   platform extension, `summon` included. Build the child's list explicitly. *(P2: the ten names
   live once, in `orchestrator.rs :: GOOSE_STRIPPED_BUILTINS`, read by `goose_agent.rs`'s strip
   guard, its `is_builtin` prompt filter and the plan builder's refusal. A source canary fails if
   `goose_agent.rs` spells one of them again. The child's list is built from what the PARENT has
   loaded, never from session or config state, and a child that ends up holding one anyway has its
   result discarded.)*
3. Concurrency is 1 for every provider that **runs on this device** until measurement says
   otherwise — `local`, `gguf`, `ollama`, `llamafile`. This used to say "`local`/`gguf`", which is
   the narrow reading PAI-4 P2 already had to fix once. See section 3.4. *(P2: one process-wide
   `Semaphore`, acquired on the only path that starts a child. **The semaphore does NOT yet cover
   the parent's turns**, which this invariant also requires — owed by P4.)* *(P4 closes the parent
   half: every on-device parent turn takes `parent_turn_permits` of the same semaphore, held for the
   whole turn by a guard the stream drops, so a child cannot reply between two of the parent's
   provider calls and overwrite the one retained KV prefix. The predicate is
   `max_concurrent_subagents` again, so a hosted provider's turns take nothing and four hosted
   conversations still run four abreast. A SYNCHRONOUS child inherits its own parent's claim rather
   than acquiring — the parent is blocked in a tool call, not talking to the provider, and acquiring
   would deadlock it against its own child while stranding an `sse_semaphore` permit.)*
   *(**P4 as first written also BROKE the child half, and this row claimed the invariant was closed
   while it was open.** Inheriting was `needed = 0`, and `acquire_many_owned(0)` never blocks; a
   device hold belongs to the SESSION, so every child of the delegating turn inherited it and N
   delegations issued in one parent turn ran N abreast on the one GPU. Repaired by giving the hold
   its own semaphore of ONE for its children to share: a child still never queues behind its own
   parent, and siblings serialise.
   `siblings_of_one_delegating_turn_still_run_one_at_a_time` reports `left: 3` against the version
   that shipped. Two lessons worth keeping: a fix for a deadlock is a concurrency change and needs
   its own concurrency test, and a status row that overclaims is worse than one that admits a gap,
   because the next phase builds on the row.)*
   *(What none of this bounds is the SSE pool: a delegating turn still holds one of `main.rs`'s
   four interactive permits for the whole of its child's run, and P5 owns that decision.)*
   *(P7 and P8 are both this invariant applied to a NEW question, and both answer it by refusing.
   A per-role model is refused on-device because a second model in the one slot is a load plus a
   re-prefill the parent pays for; a background delegation is refused on-device because with one
   agent at a time "background" only means the parent's next turn is waiting. Both predicates go
   through the same place — `ChildModel::resolve` and `max_concurrent_subagents` both ask
   `model_class::provider_locality`, and `BackgroundAvailability::for_provider` reads the
   concurrency number rather than the locality — so neither can drift into the narrow
   `local`/`gguf` reading without a named test failing.)*
   *(**Both shipped refusing too little, and the repair is dated 2026-08-11.** Each was written as
   `if runs_on_this_device(provider) { refuse } else { permit }`, which makes an unrecognised
   provider name a permission — `mock`, `lmstudio`, `llama_swap`, `omlx`, or anything the settings
   write path is handed. The invariant is about a DEVICE, and "not in a four-name deny-set" is no
   evidence about which device. Both gates now match exhaustively on a three-valued
   `ProviderLocality` and give `Unknown` the on-device answer; section 3.4 carries the reasoning.)*
4. Subagent conversations never enter the parent's `session_messages`; only results do. Satisfied
   by construction — `ChatService` is the sole writer of that table and the child loop never
   touches it — with one caveat worth stating: the child's turns *are* persisted, into Goose's own
   `sessions.db` under the child's id. "Not in the parent's history" is true; "not written down" is
   not. *(P2 closes the caveat: `ChildRunner::release` deletes the child's engine session as soon
   as the run ends, so a delegation leaves no row behind for a parent-delete to have to find. The
   parent takes back exactly one string — the text of the child's last completed assistant TURN,
   assembled by `ChildTurns` from `as_concat_text()`, which drops `MessageContent::Thinking`, so a
   child's reasoning is not an eligible result either.)* *(This row said "the last assistant
   message" until 2026-08-09, which was the shipped defect rather than the design: goose yields one
   message per provider chunk, so the last message is the last streamed word. The rule now lives in
   `child_stream_step`, a pure function beside `ChildTurns`, because the drain loop it was lifted
   out of needs a live provider — both the turn count and the text argument could be broken with
   the whole suite green, and were, by mutations that restored the original defect exactly.)*
   *(P6 adds a second thing that leaves a child and must not be persisted: an
   `AgentStreamEvent::SubagentProgress` frame. It reaches the CLIENT and nothing else — the
   `absorb` arm returns a frame and writes neither `full_text` nor `tool_results`, which are the two
   fields both stream handlers hand to `ChatService`, and the voice consumer treats it as silent.
   `absorb_progress_leaves_the_turn_untouched` fails on both natural regressions. The frame's
   `detail` cannot carry a transcript in the first place: its only producers are `child_tool_names`,
   which returns tool names, and the run's own GIAP-authored error.)*
5. Every spawn is cancellable, and cancelling a parent cancels its children. **This is entirely new
   work.** Goose's own background path mints an unrelated root token, `child_token()` is unused
   anywhere in Goose, and GIAP's parent-turn token is a stack local inside a stream closure with no
   registry and no accessor. Hence `Orchestrator::cancel_children_of`. *(P2: one
   `CancellationToken` per run in the orchestrator's registry, checked while queued and re-checked
   after the await — Goose returns `Ok(partial_text)` on cancel, so the `Result` cannot say.)*
   *(P3 closes the other end, by derivation rather than by a callback: the turn's token is
   published beside its authority, and `spawn` takes `parent.child_token()` instead of minting a
   root. So the `DropGuard` the chat stream already holds cancels the children too, and there is
   no path where a parent ends and a child does not hear about it. `cancel_children_of` stays for
   the explicit cases — a deleted session, a shutdown — and still has no caller.)*
   *(**P8 splits this row in two and the split is a decision, not an omission.** A BACKGROUND run
   outlives the turn that asked for it by definition, so it cannot derive its token from that turn —
   the chat stream's `DropGuard` would kill it the moment the parent's reply finished. It keeps its
   own, whose owner is the parent SESSION, and its cascade is `cancel_children_of` plus `cancel` by
   task id. Both are implemented and both are tested, with
   `a_synchronous_run_still_dies_with_the_turn_that_asked_for_it` as the control that P3's
   derivation is still doing its job for everything else. **The gap this leaves is that
   `cancel_children_of` STILL has no production caller**, so a deleted session or a shutdown does
   not stop a background run: one call each in `pond-api`'s session-delete handler and
   `pond-server`'s shutdown path, both outside PAI-6 P8's footprint. Until those land, "cancelling a
   parent cancels its children" is true of every synchronous child and true of a background one only
   when something asks.)*
6. Depth is capped. A recursive delegation loop on a home server is a fire. *(P1: `DelegationDepth`
   cannot be constructed from a number, deserialized, or defaulted; the only public path to a
   deeper one refuses at `MAX_DELEGATION_DEPTH`.)*

---

## 6. Deliberate deferrals

- **Nested delegation beyond depth 1.** Revisit when a real task needs it.
- **Agent-to-agent messaging.** Every use case so far is fan-out plus aggregation, which needs no
  peer channel.
- **Re-enabling Goose's `todo`.** Plausibly useful for plan tracking; evaluate separately, on its
  own merits, with its prompt text reviewed for the on-device models.
- **Learned role selection.** Roles are chosen by the model from `list_roles` in v1. A classifier is
  premature before there is usage data.

---

## 7. Verification

- **Unit** — scope inheritance: a role requesting a tool group its parent lacks gets the
  **intersection**, not the parent's set (the old wording would be a widening whenever the role was
  narrower than the parent); a role cannot express a wider profile scope at all, so there is
  nothing to deny. *(Landed in P1: `tool_narrowing_tests`, `scope_inheritance_tests`,
  `depth_tests`, and `scope_lattice_tests` on `ProfileScope::is_within`. Every one was
  mutation-tested — widening the lattice, making `Deny` return `Household`, raising the depth cap,
  turning the intersection into a union, and defaulting `grants_tool` to true all fail a named
  assertion.)* *(P3 added the axis P1 could not: the child's tool set is narrowed by the CHILD's
  scope, so `a_child_dropped_to_guest_loses_the_groups_a_guest_is_denied` fails if the subtraction
  is keyed on the parent's, and `an_inheriting_child_of_a_household_parent_keeps_memory` is its
  vacuity control — without it the first test passes against an implementation that simply deletes
  memory from every child. `the_turn_authority_is_built_from_the_published_allow_set` is the input
  guard: it reads `goose_agent.rs` and fails if the authority is built before the guest
  subtraction, from a different set than the shim's, or from a scope that is not `turn_scope`.)*
- **Budget** — with a child live, the parent's history budget shrinks by `context_fraction`; assert
  that **either** the sum fits inside `usable_prompt_tokens` **or** the effective budget is exactly
  `MIN_HISTORY_TOKENS`. The unconditional form of that assertion is wrong: `trim_history` floors
  the budget at `MIN_HISTORY_TOKENS = 64` on purpose, so at small windows or large fractions the
  sum legitimately exceeds the window. A test written from the old wording either fails against
  correct code or, if the author picks a fraction that never hits the floor, passes at every value
  tried and stays green through exactly the regression it was meant to catch. Make the vacuity
  control a case that DOES hit the floor.
- **Integration** — delegate a research task on the Orin, assert the parent's context grows by the
  result only, and compare parent context growth against running the same task inline. That
  comparison is the entire on-device justification for this workstream and should be recorded as a
  measurement in this document once taken.
- **Cancellation** — cancel a parent mid-delegation; assert the child terminates and no orphan task
  remains. **Do not assert this by checking for an `Err`**: the child loop returns
  `Ok(partial_text)` on cancellation and the literal `MAX_TURNS_MESSAGE` on budget exhaustion, so
  an `Err`-based assertion is vacuous. Assert on `TaskStatus`, which has distinct `Cancelled` and
  `TurnBudgetExhausted` variants for this reason, and on `TaskRun::result_for_parent()` being
  `None`.
- **Canary** — a test reading the fork's builtin list so that a Goose sync adding a new
  orchestration builtin fails loudly rather than silently re-enabling it. The old wording said to
  pin **both** copies of the ten names in `goose_agent.rs`; *(P2 removed the duplication instead —
  there is one `GOOSE_STRIPPED_BUILTINS` and
  `goose_agent_reads_the_one_stripped_builtin_list_rather_than_its_own` fails, with comments
  stripped so prose cannot satisfy it, if `goose_agent.rs` ever spells one of those names again.
  `the_goose_turn_cap_message_is_still_verbatim` is the submodule-reading half: it re-derives
  goose's private `MAX_TURNS_MESSAGE` from source, because an exhausted turn budget comes back as
  `Ok` with that sentence as the "answer".)* A canary over the fork's `PLATFORM_EXTENSIONS` — so a
  sync that adds an eleventh orchestration builtin fails rather than passing — is **still owed**.
- **Registration/catalog cross-check** — nothing today asserts that
  `giap_registration.rs`'s registered extensions and `tool_group.rs`'s `TOOL_GROUPS` describe the
  same set. They happen to agree at 15. P5 owes that test, because the failure mode is silent and
  it widens.
- **Per-role model (P7)** — the assertion is the REFUSAL, quantified over `ON_DEVICE_PROVIDERS` and
  over a set of providers in neither list, driven through the real `build_child_plan`, with the
  hosted case as the vacuity control. Do not
  write it as "the plan's model is `None`": `None` is also what a role that named nothing produces,
  so the assertion has to be on the `ChildModel` variant AND on the `ModelConfig`
  `child_model_config` returns. Landed as
  `a_role_model_never_reaches_the_engine_on_a_provider_that_runs_here`,
  `a_role_model_is_used_when_the_provider_runs_somewhere_else` and
  `a_role_that_names_no_model_leaves_the_parents_config_alone`. **What is still owed is a
  measurement, and it is small**: one delegation to a role naming a cheap hosted model, against the
  same delegation without one, to confirm the child really answers on the model that was named
  rather than on the provider's default. Nothing in this tree has authored such a role.
- **Background (P8)** — assert the refusal against a LOCAL provider fixture, and assert what the
  refusal did NOT do: no child engine session opened, no plan run, no reservation left behind.
  Then assert cancellation twice, because P8 has two cancellation stories and the interesting test
  is the pair — a background run must SURVIVE its parent turn ending and still stop on
  `cancel_children_of`, and a synchronous one must still die with the turn. The second is the
  vacuity control for the first: without it, an implementation that stopped deriving anything from
  the turn passes. **What is owed is not a test**: `cancel_children_of` has no production caller, so
  no test in any crate can show a deleted session stopping a background run, because no code path
  does it.
- **`list_roles` has no test because it has no code.** It is in section 3.6's tool list and no phase
  built it; see that section.

---

## 8. Long-running tasks (designed 2026-10-28; P9-P13)

### 8.1 Why one window cannot hold them

A window on the Orin is at most `MAX_CTX` = 16,384 tokens (`device_budget.rs`), and that cap is set
by latency rather than memory: a cold 16K prefill takes about 20 s. A task that needs more context
than one window holds cannot be one conversation. It runs as many short windows, one after another,
with everything it learns kept on disk. The task's working memory is its store, not a KV cache, so a
task resumes after a restart with nothing to re-derive.

### 8.2 Shape

| Part | What it is | Who decides |
|---|---|---|
| Task | Goal, owner, budget, status; rows in `pond_system.db` | The member who started it |
| Plan | Steps: role, instructions, input artifacts, acceptance check, dependencies | The planner role, through `plan_write` |
| Step runner | Picks the next ready step, runs its helper, records the result | Code, on the inference lane |
| Helper | A saved role. One step is one helper life in a fresh window; nothing of it is kept | The model |
| Artifact | A step's output: typed, titled, token-counted, scoped to the task's owner | Written by helpers |
| Prime | The household's conversation: starts a task, reports on it, asks for decisions | The model |

The runner is code, not a model, so the depth cap (invariant 6) and "only the prime talks to the
household" both hold without a nested agent.

### 8.3 Rules

1. **The model decomposes.** The planner writes the plan with a tool call; nothing splits a goal by
   keyword. The runner only follows the plan's dependencies.
2. **Pull, don't push.** A step is given artifact ids with a one-line summary each, never their
   bodies. It pages in what it needs with `artifact_read(id, from, to)` and `artifact_search`.
3. **A fixed hand-back.** Every step returns its outcome, a capped summary, the artifacts it wrote
   and what failed. The planner and the prime see that shape, not a helper's prose.
4. **Checked, then replanned.** A step with an acceptance check is verified by a verifier role. A
   failure retries once with the reason, then goes back to the planner with the results so far,
   then to the household.
5. **One at a time, at the household's pace.** Steps are a lane job: none starts while a turn is in
   flight, and a step is the unit of pre-emption. A step stopped by a household turn reruns from the
   ledger, so steps record outward effects before they happen and never repeat one.
6. **Outward effects wait for a person.** A step whose role holds a tool that sends, buys or moves
   something parks the task as awaiting approval, and the member is asked on their own phone
   (PAI-2's gate), never in the helper's window.
7. **Visible and stoppable.** The task, its plan, each step's state and its artifacts are on screen;
   a member can pause, cancel or edit the plan, and milestones reach their phone.
8. **Budgets.** Each task has a step cap and a wall-clock cap; quiet hours and the device's thermal
   state pause it.
9. **Scope.** A task runs at its owner's scope (PAI-1) and its artifacts inherit it. Memory
   extraction never reads helper turns (invariant 4).

### 8.4 What it reuses

- PAI-7's reviewer already runs a child from the lane through `Orchestrator::spawn`, under a root
  authority it registers as the run's parent turn (`review_authority`, `plan_review`). The step
  runner is that loop, generalised to a plan.
- Roles are `agent_recipes` rows parsed into `AgentRole`; concurrency 1, child history never
  reaching a parent, and child sessions deleted on release are invariants 3 and 4.
- `delegate` with `background: true` stays refused on this device. The lane runner is how background
  work runs here: it waits for the device instead of holding it.
- Goose's `todo` stays stripped (section 6): `plan_write` is the plan tool, with its text written
  for the on-device models.

### 8.5 The engine: one context, handed over

A helper's prompt is short, so `prefill_plan` sends it to `SacrificialContext`: a second context
beside the prime's, decoded from nothing, with no snapshot. On the Orin a context costs about
522 MiB of compute buffers before any KV (`device_budget.rs`), against roughly 0.5 GB the serving
pond has free. The alternative hands the helper the one context, seeded from its role's snapshot,
and rebuilds the prime afterwards: from its preamble snapshot plus the history, or from a suspend
file. `what_a_helper_turn_costs_the_prime` (goose `inference_engine.rs`) measures the three.

Measured on the Orin Nano 2026-10-28 (E4B QAT UD-Q4_K_XL with its MTP drafter, `n_ctx` 16384, f16
KV, flash attention, the pond stopped). The prime is a 3,000-token preamble plus history; the helper
is a 1,100-token role preamble plus a 300-token task.

| History | Prime cold | Side context | Helper from its snapshot | Prime rebuilt | Suspend file |
|---|---|---|---|---|---|
| 2,048 | 8.1 s | 2.2 s | 0.5 s | 3.7 s | 1.7 s write, 0.7 s read, 99 MiB |
| 4,096 | 11.6 s | 2.0 s | 0.4 s | 7.2 s | 0.8 s write, 0.6 s read, 131 MiB |
| 8,192 | 19.2 s | 2.0 s | 0.4 s | 14.8 s | 1.0 s write, 0.6 s read, 195 MiB |

A rebuilt and a resumed prime predict the same next token as a cold one at every length. The side
context's memory read 160-295 MiB from `MemAvailable`, which page cache makes noisy on unified
memory; it is a bound to re-measure, not a constant.

What it means:

- **Long tasks pay almost nothing.** No prime is waiting between steps, so each step is a helper
  seeded from its role snapshot (0.4-0.5 s), and the household's next turn rebuilds the prime once,
  on the cold path that already exists.
- **A delegation inside a live turn costs the rebuild,** which grows with the history: 15 s at 8K.
  A suspend file holds that to about 2-3 s at any length, but it writes the conversation's KV to
  disk for as long as the helper runs.

**Decided 2026-10-28 (Jerry): the suspend file, bounded.** It is written only when a live turn
waits on the helper (`request_context::as_helper(prime_waiting, ..)`, true when the child inherits
its parent's device hold), never for a lane step. It is owner-only (0600 in a 0700 directory under
`<goose data>/prompt-cache/suspended`), never indexed, and deleted on resume, on any call that has
no waiting turn, when the model unloads and when a process starts. Its window checkpoints stay in
memory, so a resume rewinds through the same `window_resume` path a resident cache uses.

Role preambles also need their own snapshots. The ladder keeps `MAX_SNAPSHOTS` = 6 per model and
window, and the Orin's E4B family already holds six (typed chat, voice, extraction, titling, the
reviewer, the summariser), so roles would evict them. A role's snapshot is written when the role is
saved, as an idle lane job, not on its first step.

### 8.6 Phases

- **P9 — engine.** Helper turns take the one context and the prime is rebuilt by whichever path the
  bench picks; role snapshots get their own budget. **LANDED 2026-10-28**, measured on the Orin
  (8.5).
- **P10 — the ledger.** Domain types, migration and repository for tasks, steps and artifacts.
  **LANDED 2026-10-28** (migration 0062).
- **P11 — plan and run.** The planner, verifier and step runner; `plan_write`, `artifact_read`,
  `artifact_search`, `artifact_write` for helpers; `task_start`, `task_status`, `task_cancel` for
  the prime; the lane job. **LANDED 2026-10-28; VERIFIED on the Orin**: a four-step task ran each
  step once under the verifier, wrote a 620-token result and notified the owner's devices.
  `artifact_search` was not built: helpers page artifacts with `artifact_list` and `artifact_read`.
- **P12 — the household's controls.** Approvals, the task panel in both UIs, phone milestones.
  **PARTIAL.** A task's owner is notified when it ends, and the chat that started it shows a live
  card with its steps, its result and Stop (desktop; the phone in goose-on-the-go #199). The
  approval path is not built, so no step can act outward.
- **P13 — on the Orin.** One task longer than any window, run end to end with the pond serving.
  **OPEN**: the Orin's task fit in one window.

### 8.7 P11's seams

- **Two extensions.** `giap-tasks` is the prime's (`task_start`, `task_status`, `task_cancel`),
  withheld from guests and from subagents, so a helper cannot start a task. `giap-task-work` is
  the helpers' (`plan_write`, `artifact_list`, `artifact_read`, `artifact_write`, `step_report`),
  withheld from guests and subtracted from every top-level turn's allow set and entitlement. Only
  the runner's root authority grants it.
- **Who is calling.** A helper's call carries its own engine session id.
  `process_child_sessions().parent_of(id)` gives the runner's root session for that step, and
  `process_step_bindings().binding_for(root)` gives the task, the step and the owner's scope. A
  call that resolves to nothing is refused in one sentence.
- **What the tools write.** `artifact_write` and `plan_write` go straight to the ledger
  (`plan_write` validates first and replaces only open steps). `step_report` is recorded on the
  binding, and the runner applies it after the run, so state transitions have one author.
- **The runner.** A lane loop modeled on `run_proactive_reviewer`: it claims `LongTaskStep`, takes
  the slot only when a task is runnable, runs one helper per slot, and cancels on household
  activity. Which helper to run next is a pure function in `pond-core`: the planner for a task
  in `Planning` or with blocked steps, else the next ready step's role, then the verifier when
  that step has an acceptance check.
- **Outward effects.** Subagents are already denied device control, notifications, scheduling,
  rides and cameras (`groups_denied_to_subagents`), so in P11 no step can act outward at all. P12
  adds the approval path that lets one do so.

### 8.8 Deep research (Jerry, 2026-10-28)

A member can ask for research from the chat composer's + menu, on the phone and the desktop,
instead of waiting for the prime to decide a question needs a long task.

| Piece | Design |
|---|---|
| Start | The turn is sent with `deep_research`. The pond starts the task itself, with the message as its goal, rather than relying on a small model to call `task_start`. |
| Owner | The caller the long-task routes answer: the device's member, or the primary member on the pond's own desktop. So the chat card can always read the task it started. |
| Record | `drive_turn` folds a `task_start` call and its result through `absorb` before the engine's events. The stream and the stored tool rows are those of the prime's own call, so the card anchors live and in history. |
| The prime | The turn's `<system-context>` says the research has started, so the reply acknowledges it in a sentence or two and doesn't answer unresearched. |
| Research | `LongTaskKind::Research` (migration 0067). The planner is told to gather with `search_web` and `get_wikipedia_article` before one step writes the `Result` report. Workers keep each source's title and link beside what it says. The helpers' tools are unchanged: `giap-knowledge` was already theirs. |
| Refusals | `GET /long-tasks/deep-research` says whether this caller may use it, and the composer hides or disables the item. The stream refuses before saving anything: 409 when long tasks or the orchestrator are off, 403 when nobody is identified. |
| Start time | The runner picks the task up on its next poll (30 s) once the household has been quiet for 30 s, as for any task. |
| Length | A report is not one artifact a worker writes in one reply. Research tasks get 24 steps; the planner gathers, then gives each section a step that saves it as a draft, then writes the `Result` summary. `research_report::assemble` joins the summary and the drafts in plan order, then lists the pages they cite, so a report is as long as its plan and no context window holds it whole. |
| Plans | A shipped `long-task-composer` role builds itineraries, meal plans and lists: a heading per day, time lists, tables, checklists, map links, the forecast, and what the household has said about who is coming and what they eat. Dated events go to a `Calendar` data artifact. Neither the composer nor a research step puts a person's name, health or diet in a web search. |
| Export | `GET /long-tasks/{id}/report` gives the joined report and its sources. `GET /long-tasks/{id}/export` gives Markdown; a standalone page that prints or saves as a PDF, with every bit of markup the model wrote escaped, links only to web and mail addresses, and nothing fetched; or an iCalendar file of the plan's events. |
| Names | The shipped helpers are shown as Point Gander (planner), Forager Gosling (worker), Sentinel Goose (verifier) and Nestmaker Goose (composer), and each recipe tells its helper its own name, so the planner sees the others' too. Their ids (`long-task-planner` and so on) are unchanged, since plans, snapshots and the prefill registry route by them. The main assistant keeps the name the household gives it. |
| Pictures | None are generated: the Orin runs no image model. A report's pictures are links, and the export never loads them. |

### 8.9 The line (Jerry, 2026-10-29)

One slot works every open task, so a task asked for while others are open waits. A member was left
looking at a card that said "Planning" for an hour and asked twice more. Now the pond says where each
task waits and lets the member choose what keeps going.

| Piece | Design |
|---|---|
| Order | `long_task_queue::run_order`: open tasks in the order they were asked for, and the runner finishes the front one before it starts the next. The runner, the routes and the prime all read it, so none of them can disagree. It replaced a pick by `updated_at`, meant to let tasks take turns. It did not: a step never touches `updated_at`, so the oldest task always won. |
| Who sees what | A member sees the goal and progress of their own tasks and the household's. Another member's task is only a place in the line (`another_member`), never its goal. |
| Routes | Every task's view carries `queue: {position, of, ahead}`, `null` once it has ended. `GET /long-tasks/queue` gives the whole line as the caller may see it. Stopping is `POST /long-tasks/{id}/cancel`, as before. |
| The prime | A deep-research turn and `task_start`'s result say where the new task waits and whose tasks are ahead, so the reply tells the person it waits its turn and that its card lets them stop what they no longer need. |
| The card | A waiting task says "Waiting its turn: 3rd in line" rather than "Planning". Below that are the member's own tasks ahead and this one, each with Keep going or Stop, and one button to stop the ones picked. |

### 8.10 Pace (Jerry, 2026-10-29)

Research has to stay deep and checked and get faster. Every change here removes waiting and
re-reading. None removes a check.

| Piece | Design |
|---|---|
| The lane | `select_next` gave the slot to whichever eligible job had waited longest, so a long task, which runs most often, lost every gap between steps to a chore. A member's task now goes before the pond's chores, after only a compaction a returning member waits on. A chore that has gone `CHORE_PATIENCE` (30 minutes, §8.12) since its last run still gets one turn. |
| Background subagents | The lane's "turn in flight" now also counts a held subagent permit. A background child outlives its parent's turn, and a chore used to start beside it. |
| Between steps | A run that moved a task on is followed at once, with no 30 s poll and no 5 s floor, so a chore can't take the gap. The quiet before a task starts is 10 s since §8.13. |
| Inputs | A step's inputs up to `INLINE_ARTIFACT_TOKENS` each and `INLINE_TOTAL_TOKENS` in all go in its brief whole. A worker then spends no turn and no re-prefill calling `artifact_read` first, and a small model can't skip reading them. Larger inputs stay pull-only (rule 2). |
| Reliability | A helper that runs out of turns fails its first try instead of having half-done work counted as done. Its retry gets half again as many turns (capped at `MAX_ROLE_MAX_TURNS`), with what it saved inlined so it continues. A "completed" report with no artifact and a summary under 20 characters fails as empty and retries. The verifier, the retry and the planner's replan are unchanged. |

### 8.11 Briefs (Jerry, 2026-10-29)

A helper starts in an empty window and saw only the goal, so anything said earlier in the
conversation was lost. "I'm lactose intolerant", said three messages before the ask, never reached
the plan.

| Piece | Design |
|---|---|
| The brief | A task-level `Brief` artifact (kind `note`, no step), saved when the task starts. It is in the planner's brief and inlined into every step's, ahead of the step's own inputs. No migration: it's an artifact like any other, scoped to the task's owner |
| Deep research | The pond writes the brief itself from the conversation before the ask, up to `BRIEF_CHARS` (4,000), keeping the newest. The ask itself is the goal. No model call |
| `task_start` | Takes `brief`. Its description asks for who it is for, dates and places, budget, diets and allergies, and what was decided or ruled out |
| `delegate` | `instructions` asks for the same: a full brief, since the agent sees nothing of the conversation |
| Saved or made agents | A plan step names a saved role, or carries `new_agent` (name, instructions, tools) when none fits. It is stored with the step (migration 0071) and named `new:<name>`. `role_for_agent` builds it from the shipped worker's turns and window share. Its tools are those it asked for within `LONG_TASK_ROOT_GROUPS` and the house's read-only tools (§8.12), always with the task-work tools. It runs on the conversation's model with the task's scope. It has no compiled snapshot, so its first run starts cold |
| `delegate` with `new_agent` | The prime may make an agent for one request. `made_role` builds it, and `DelegationAuthority::delegate` narrows its tools to the prime's own, as for any delegate. The prime still can't choose a model |
| Fuller sections | A research step writing a section is asked for 400 to 900 words of specifics, and a plan for every day or item with its times, places and costs. This is in the step's brief, not its role, so no snapshot is rebuilt |

### 8.12 Plans that read the house (Jerry, 2026-10-29)

A deep-research question about what the cameras saw ran 23.7 minutes. 10 of them went to planning,
where 8 plans were refused, 7 of them for fields a step doesn't read. The steps then searched the
web for what happened in the house, because no helper could read the house, and the result said
nothing useful.

| Piece | Design |
|---|---|
| Plans | A step's unknown fields are ignored, and `plan_write`'s answer names them. A field spelt like one a step reads (`dependsOn`, `dependson`) is read as that field, since a dropped `depends_on` costs the step its inputs (`read_step_draft`). A key that is only misspelt is taken as its suggestion (`step_1` is `step-1`), and `depends_on` is matched the same way. One refusal names every unreadable step |
| Refusals | Every refused long-task tool call is traced with the first 400 characters of what the model was told. Goose doesn't keep a helper's session, so this is the only record |
| Reading the house | `HOUSEHOLD_READ_TOOLS` are single tools a long task's helper may hold from groups no subagent holds whole: `get_recent_camera_events`, `list_vision_automations`, `list_schedules` and `list_registered_devices`. They show no camera picture, send nothing, and schedule or switch nothing. Only `long_task_authority` grants them, through `with_read_tools`, which keeps nothing outside that list. A child gets one only when its role asks for the tool's group, and a guest child gets none that guests can't see. A chat turn's delegations get none. The worker asks for vision, schedule, device and sensors |
| Where answers live | The planner and every research step are told to decide first whether an answer is in the house or in the world, and to search the web only for the world |
| Out of reach | When part of the answer is out of every helper's reach, such as old footage or a picture, the plan has no step for it, and the Result says what is missing and how the person can get it |
| Relevance | The verifier is given the task's goal and brief, and fails a step whose work doesn't serve them, including web results for a question about the house |
| Save and report | `artifact_write` takes `finish`. On the last save it also reports the step completed with the save's summary and every artifact the run saved. This saves the turn a helper spent calling `step_report` |
| Nudge | A step or check helper that stops with turns left and no report is sent one more message in its own session (`REPORT_NUDGE`, at most `NUDGE_TURNS`). It costs a short decode on a warm cache, where a retry re-reads its brief and redoes the step. A helper out of turns isn't nudged: its retry gets more turns |
| Chores | `CHORE_PATIENCE` is 30 minutes. A chore's turn costs a task its slot for about a minute, and a deep-research task runs about 20 |
| The prime's cold turns | Goose's `SNAPSHOT_MAX_TOKENS` is 10,240, above the chat preamble. E4B stores about 20 KiB a token, so the snapshot is about 200 MB, written once. At 6,144 a cold chat turn re-decoded the rest of the preamble |

### 8.13 The answer the person wants (Jerry, 2026-10-29)

The pipeline answered every ask with a sectioned report, so a question with a one-line answer paid
for gathering, sections and a summary, then showed one line and a sheet to open. Long tasks are the
base other features build on, so length is now a property of the task. The person decides it, or
the pond reads it from the ask.

| Piece | Design |
|---|---|
| The shape | `AnswerShape` on the task (migration 0073): `auto`, `short` or `report`. `short` is the answer itself in a few sentences, at most 150 words with its sources. `report` is sections with a Result that opens them |
| Who decides | The member, beside the deep-research chip on the desktop and the phone ("Pond decides", "Short answer", "Full report"). Or the prime, through `task_start`'s `answer`, when the person said how much they want. Otherwise the planner reads the ask and passes `answer` to `plan_write`. Report is the exception it names: a report, a plan, a comparison of many options, or thoroughness; a softer "short when unsure" gave the same question both shapes across runs. `decide_task_answer` changes only an `auto` task, so a member's choice stands |
| Report sections | A report plans only the sections the goal asks for or cannot do without: given the composer's examples, it added a meal plan and packing list to a weekend nobody asked one for. A Result as long as its sections, from a last step that compiled them, is not followed by them again (`holds_its_sections`) |
| A chosen short answer | `open_task` stores the task paused with one worker step (`short_answer_plan`), then sets it running. No planner runs |
| Briefs | `answer_plan` and `answer_step` size the planner's and the workers' briefs. A short plan has one or two steps and no sections, and `plan_write` refuses a longer one (`MAX_SHORT_STEPS`): told so, the planner still once wrote three, and the answer took 5.9 minutes instead of about 1.5. A report keeps its sections and their 400 to 900 words |
| The answer's home | The pond tells the plan's last step to title its answer "Result" (`last_step_line`), whatever the planner wrote. A short task with no Result takes the newest thing its steps saved (`answer_artifact`): a helper told to title its answer Result called it "Current Presence and Camera Status". One that saved nothing and put its answer in its summary keeps that summary as its Result (`summary_as_result`) |
| The card | The task view carries `answer` and, for a finished short answer, `answer_text`. The card shows the answer whole in place of the result line and the report sheet. The notification carries it too, links read as words, cut at 700 characters |
| Names | A plan step with no key takes one from its name or its place (`step-2`), and an unreadable step's refusal names the fields it had. A wait may name a step by its name, title or number (`match_waits_by_name`). One that matches nothing waits for every step before it (`RelaxedWait`), and `plan_write` says so, rather than refusing the plan. A `new_agent` that can't be read as one (a string, or an object with no name) is dropped, and a step with neither a role nor an agent goes to the general worker. `step_report` reads an artifact named by its title as that artifact, and a made-up id as what the run saved |
| Sources | A link saved to a bare web address gets `https://`. A camera with no events reads the household's camera and says so, since a helper asked about an imagined "front door" camera and gave up |
| Tools by job | `tools_withheld` keeps each helper to the task tools its job uses: the planner is not offered `artifact_write` or `step_report`, the checker not `artifact_write` or `plan_write`, a worker not `plan_write`. The planner used to try the step itself first, a turn each time. The prefill compile withholds the same, so each role's snapshot still matches its runs |
| Correctness | A question about the house plans no web step, and research looks only where the goal points. A helper writes only what its tools returned, and never that the house has nothing unless the house's own tool said so. The verifier fails off-goal work (§8.12) |
| Start | Every `open_task` rings the runner. A task waits 10 s of quiet, not 30, and the runner looks again when the quiet is due rather than at its next 30 s poll. A step the household interrupts reruns without spending a try |
| The prime's turn | The deep-research note promises the answer that will come and asks for no tool call: the prime used to look up the task it had just been told about |

### 8.14 Reading the pages it cites (Jerry, 2026-10-30)

A search quotes a few passages a page and no address, so a helper wrote prices and times from
snippets: one short answer gave the train from Nairobi to Mombasa as 1 h 26 min. Forager Gosling
now reads the page with `read_web_page` ([small-model-tools §9](../small-model-tools.md#9-reading-a-web-page-read_web_page)).

| Piece | Design |
|---|---|
| The tool | `giap-knowledge__read_web_page(url, question, part?)` fetches one page through the search's public-only client. It turns the page into Markdown lines, in parts of about 800 tokens, and returns the lines that answer the question, or a part whole by its number. No key and no model call |
| Who reads | Forager Gosling's recipe names it beside `search_web`. The research step reads the page before writing a number, price, time or date from the web, and cites the address the tool gave. Nestmaker Goose and the chat turn hold it through `giap-knowledge`. The planner and the verifier do not |
| Which pages | Only an address the person wrote, a task's goal names, or a search returned. A helper holds the memory tools, and a page could tell it to "read" an address carrying what it knows. The runner notes the goal's links on every pass, so a long task can read them however long it runs. A helper names a search source by its domain, as the copy shows it |
| Cost | The tool block grows by 666 raw bytes, so every helper's snapshot is compiled again once. Each read is one more model turn: helpers that read two or three pages a step doubled a weekend plan's research time on the Orin, so a step reads one page, two at most, and one more part at most |
| Work left in a reply | A composer wrote its draft as a reply, answered the report nudge with a `step_report` that was refused for saving nothing, and lost the step, twice in two reports (3 to 5 minutes and a replan each). The nudge now names `artifact_write` first. And when a step's helper reports nothing and saves nothing, a reply of 60 words or more is saved as the step's draft (`reply_as_draft`), titled "Result" on the plan's last step. A nudged helper hands back the longer of its two replies (`after_nudge`) |
| Gathering | A report gathers in one step per thing the question is about, such as each option a comparison weighs, not one per question about it: a train-against-bus comparison was planned as six gathering steps and took 20 minutes |

---

## 9. Agents are recipes (designed 2026-10-28; P14-P18)

A household's agents are `agent_recipes` rows: Goose recipe YAML with a `giap_role` block, which
`AgentRole::from_recipe_yaml` already reads, run by the existing routes (`/recipes`,
`/recipes/{name}/run`), the scheduler and the Recipes screen. The builder grows that block; it adds
no second store and no second runner.

### 9.1 The block

```yaml
giap_role:
  tool_groups: [giap-knowledge, giap-weather, giap-task-work]
  personal_data: inherit            # or deny: the agent sees no personal data
  max_turns: 8
  instructions: |
    What the agent is for, in plain words.
  context:                          # what goes into its preamble, and so into its snapshot
    examples:                       # few-shot pairs: free after the prefill is compiled
      - brief: ...
        answer: ...
  activations:                      # when it runs; none of these reads a user's words
    - delegated                     # the prime may hand it work (default)
    - task_step                     # a long task's planner may give it a step (default)
    - schedule: { cron: "0 0 7 * * MON-FRI", brief: "..." }
    - event: { on: presence.arrive, member: owner, brief: "...", cooldown: 30m }
    - event: { on: camera.person, camera: any, between: "22:00-06:00", brief: "..." }
    - agent: [trip-planner]         # these agents may hand it work
  hooks:
    before: [today, weather]        # context gatherers from a fixed catalog, never code
    after: [notify_owner]           # or save_artifact, start_task, hand_to: <agent>
  a2a:
    expose: false                   # offer it to paired peers over the A2A protocol
```

### 9.2 Rules

1. **Activations are structured, never keyword matches.** `delegated` and `task_step` are model
   decisions; `schedule` and `event` are explicit triggers on the scheduler and the event bus
   (sensor, camera, device, presence, time), each with a cooldown. `member: owner` is the recipe's
   owner, so one member's agent never greets or notifies another; `between` holds an event to part
   of the day on the household's clock, wrapping past midnight, and an event outside it spends no
   cooldown. Nothing reads a user's text.
2. **Every run is a lane run.** A scheduled or event activation queues a single-step run on the
   inference lane; it never interrupts a household turn and never runs beside it.
3. **Hooks are a fixed catalog.** `before` hooks add context to the brief (the date, today's
   schedule, the weather, the latest presence). `after` hooks act on the hand-back. An outward hook
   (anything that sends, buys or moves something) waits for the member's approval on their phone,
   as long-task steps do (P12); `notify_owner` reaches only the owner's own devices.
4. **Agent to agent is mediated.** An agent's hand-back may ask for `hand_to: <agent>` with a
   brief. The runner (code) runs that agent next with the first one's artifacts, so depth stays 1,
   only agents named in `activations.agent` accept, a chain is capped at 4 hand-offs, and a cycle
   is refused. Agents never message each other directly.
5. **External A2A is off by default.** `a2a.expose` publishes an agent card and accepts A2A tasks
   from paired devices and mesh peers only, through `auth_middleware`; every A2A task is a lane run
   at the exposing member's scope, and its result leaves through `pond-net`.
6. **A saved agent is compiled.** Saving a recipe compiles its prefill (9.3). The builder shows the
   preamble's token count, whether its snapshot is valid, and what a cold start would cost.

### 9.3 Compiling every agent's prefill

On every start, and when a recipe is saved, the pond checks each agent's snapshot and rebuilds only
the ones that are missing, unreadable, corrupt or stale. The shapes: the prime's typed and voice
turns, and every role in the shape a lane run gives it (`LONG_TASK_ROOT_GROUPS`).

The engine is the judge, because it is what a real turn trusts. A compile is two calls of one shape
whose user messages differ from their first token, under `request_context::compiling_prefill`:

- The first call runs cold and records whatever snapshot it restored.
- The second call learns the stable preamble from the two prompts. The snapshot is **valid** when
  it restored exactly that preamble and its checksum matches the one written beside it in the
  index. Anything else is rebuilt: **missing** (none matched), **unreadable** (llama.cpp refused
  it), **corrupt** (the checksum differs) or **stale** (it covers less than the preamble).
- A compile call decodes one token at most and never resumes a chat's cache.
- Nothing else may reach the engine under the scope, or it becomes the pair's first call. So the
  engine admits a prompt by its window alone: a refused warm-up sends goose into a recovery
  compaction, which runs inside the scope.

The boot pass is a lane job, exempt from the activity gate, after the existing prewarm. The
household's turns still come first.

### 9.3.1 Which prefills are kept (Jerry, 2026-10-28)

| Agent | Prefill |
|---|---|
| Default agents (the shipped planner, worker and verifier) | Compiled on every start. |
| Suggested agents (shipped templates) | The recipe exists; no prefill until a member turns one on. |
| An agent a member builds or turns on | Registered with its definition's hash in `agent_prefills`, compiled when it is created or edited, and rebuilt on any start that finds it missing or bad. |
| An agent unused for `agent_prefill_retention_days` (14) | Its snapshot is discarded and the start-up pass skips it; its next run compiles it again, cold once. |

Use is recorded whenever the runner gives the agent a step. The engine applies the retention to
helper snapshots on every compile pass, by their own last use, so a snapshot no registry row
claims is not kept forever.

### 9.4 Phases

- **P14 — prefill compile.** The engine primitive, checksums in the snapshot index, and the boot and
  on-save compile passes. **LANDED 2026-10-28.** The Orin found that snapshot files were named by
  their first 256 tokens, which the prime and every role share, so each compile overwrote the last;
  files are now named by their whole prefix and replaced only by their own shape.
- **P15 — the block.** Parsing and validation of `context`, `activations`, `hooks` and `a2a`, plus a
  validate route that names the field a small model or a person got wrong. **LANDED 2026-10-28.**
- **P16 — activations and hooks.** Schedule and event activations as lane runs, the hook catalog,
  and mediated hand-offs. **LANDED 2026-10-28; NOT VERIFIED on hardware**: no schedule or event
  activation has fired on the Orin.
- **P17 — the builder.** The Recipes screen in both UIs: a form over the block and a YAML view, a
  test run, compile status and preamble cost. **LANDED 2026-10-28** on the desktop, with each
  agent's prefill state and a rebuild; the phone's is goose-on-the-go #198.
- **P18 — external A2A.** Agent cards and A2A tasks for exposed agents, off by default.
  **DEFERRED**: `a2a.expose` is parsed and validated, but no agent card or A2A route exists, so an
  exposed agent is reachable from nowhere outside the pond. It costs nothing until a second pond
  or client needs one.
- **P19 — suggested agents.** Shipped templates offered as suggestions, never switched on (9.5).
  **LANDED 2026-10-28; VERIFIED on the Orin**: the cards listed, and one tap made a member's copy
  that the next pass compiled.

### 9.5 Suggested agents

The pond ships agent templates and offers them; none runs until a member turns it on.

| Piece | Design |
|---|---|
| Templates | Shipped recipes beside the planner, worker and verifier, read-only: a copy may narrow their tools, never widen them. |
| Where they show | Suggestion cards on Home and the Recipes screen, from `GET /agents/suggestions`; enabling and dismissing are its two `POST` routes. |
| When a card shows | Only when the pond can run it: welcome back needs presence and an identified member, camera follow-ups a camera, a tank alert that sensor. |
| Turning one on | One tap copies it into `agent_recipes`, owned by that member, and compiles its prefill then, so its first run is warm. The builder edits the copy. |
| Nothing automatic | A card never activates itself. Schedules and events start after the tap, and only with `ext_orchestrator_enabled` on; a schedule's first sighting only sets its baseline. Anything outward still waits for approval. |
| Dismissing | A declined card stays hidden, recorded like the proactive reviewer's ledger. |
| Cost | A compiled preamble is about 20-30 MB on disk, paid only for templates turned on. |

The first set: morning brief, welcome back, late-night gate alert, low-tank alert, trip planner,
document reader, study planner.
