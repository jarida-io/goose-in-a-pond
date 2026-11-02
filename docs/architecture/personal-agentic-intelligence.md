# Personal Agentic Intelligence — programme roadmap

State of the world verified against code on 2026-08-03 and re-audited 2026-08-04 (section 1.4 and
the status table below carry the corrections), and the design programme for the eight
capabilities that turn GIAP from a very good reactive assistant into a personal agentic
intelligence.

Before working on any item in this programme, read [`pai/00-checklist.md`](./pai/00-checklist.md) —
the standing checklist and running scratchpad. It is the entry point; this file is the reference.

> Status (2026-10-23): the table in section 2 is checked against the code; the checklist's section 0
> has the same reading with the next step for each workstream. Section 1 is the tree as it stood when
> the programme began (2026-08-03), kept as the reason for the programme, not as current state.

All `file:line` references are to this repository unless prefixed `goose/`, in which case they are
to the fork at `jarida-io/Goose:main`. The submodule is **not initialized** in a fresh clone
(`git submodule status` reports `-29ef609c…`), so Goose-side claims here were verified against a
separate checkout; run `git submodule update --init --recursive` to re-verify locally.

---

## 1. Why this programme

GIAP answers well. It selects tools, remembers, forgets on a schedule, sees images, narrows its
tool surface per session, and survives a restart with its conversation intact. Phases A-F of
[context-and-reasoning-roadmap.md](./context-and-reasoning-roadmap.md) landed, and they are the
reason the assistant is usable on an 8 GB Jetson at all.

What it does not do is act like it belongs to *someone*. Four spines are missing, and every one of
the eight requirements below is blocked on one of them.

### 1.1 There is no subject

`Profile` exists (`crates/pond-core/src/user_data/domain/profile.rs`) with six fields and a
CRUD repository. Everything downstream of it is unwired:

- `MemoryFragment.profile_id` exists (`user_data/domain/memory.rs`) and `sqlite_memory.rs` branches
  on it — but **every production call site passed `None`**: `goose_agent.rs`,
  `pond-mcp-server/src/memory.rs`, `user_data/services/memory_extraction.rs`. One household, one
  memory pool.
  **PARTLY FIXED 2026-08-04 by PAI-1 P1.** The five search methods take `&ProfileScope` now, so
  "everything" is a deliberate, greppable act. Every call site still says `Household`; narrowing
  them is P3 and P4.
- ~~`Session` has no profile column at all.~~ **Wrong, and the truth was worse.** The column has
  existed since migration `0003`, unwritten and unread for thirty-four migrations — a dead column,
  the same failure as `memory.profile_id` and the identification map below.
  **FIXED 2026-08-04 by PAI-1 P2**, which also had to add a delete trigger: the column's untested
  `NO ACTION` foreign key would have started failing member deletion the moment anything wrote it.
- `POST /sessions/{id}/identify-user` wrote into `AppState.session_user_bindings`, an in-memory
  `HashMap` touched only by its own three handlers. **Nothing on the chat or prompt path read it.**
  **FIXED 2026-08-04 by PAI-1 P2** — the map is deleted and the three handlers read and write the
  session row, so a binding now survives a restart. Still nothing on the chat path consults it;
  that is P3.
- Only `settings.primary_profile_id` reaches the model (`routes.rs:1191-1205`).
- Speaker identification is a design document ([data_pipeline.md, Biometric User Differentiation](data_pipeline.md#biometric-user-differentiation)) — no
  port, adapter, table, migration or crate exists.

### 1.2 There is no enforcement

`SecurityPolicy` is a well-designed port with eight scopes and a `Principal` type
(`security/ports/policy.rs:38-111`). Both implementations return `Ok(true)` unconditionally
(`security/services/policy.rs:27-28`, `pond-infra/src/sqlite_security_policy.rs:62-64`), and every
single `.audit()` call site in the repository is inside a `#[cfg(test)]` module.

Alongside that: no general-purpose redaction exists anywhere (the only redaction in the tree is
`pond-infra/src/push_token_log.rs`, which shortens push tokens for logging); there is no encryption
at rest; and `api_key_guardian` / `_gnews` / `_finnhub` / `_coingecko` are plain `Option<String>`
fields on `Settings` carrying only `#[serde(default)]` — so `GET /settings`, which serializes the
whole struct, **returns them**.

**And it used to return them to anybody.** Found during the 2026-08-04 audit, not in the original
pass: `is_public_route` matched on the request path alone — `auth_middleware` never passed it the
method — while its entries were commented as though method-scoped ("PUT /settings is public so
onboarding can save", "POST — create profile during onboarding").
`public_routes.merge(protected_routes)` then puts everything behind that one check, so the
`protected_routes` label decided nothing. The consequences were `GET /settings` (every API key) and
`DELETE /profiles/{id}` reachable **with no token at all**.

**Fixed 2026-08-05** — [PAI-2](./pai/02-privacy-and-security-guardrails.md) P0. The allowlist is now
a `(Method, path)` table with segment-wise wildcard matching, and three compile-time guards fail the
build if it and the router ever disagree. The secrets-on-`Settings` half of this section is
untouched and remains PAI-2 P2: the fix stops `GET /settings` being *reachable*, not the keys being
*on the struct*.

> **Three more of this section's claims went false on 2026-08-05 and were not struck through at the
> time. Corrected while landing PAI-2 P1**, because a roadmap that overstates the danger is read
> the same way as one that understates it — sceptically, and then not at all.
>
> - **"Every `.audit()` call site is inside `#[cfg(test)]`" is no longer true.** There are two
>   production call sites: `evaluate_identity_assertion` in `pond-api/src/routes.rs`, and the draft
>   decision gate in `pond-mcp-server/src/draft.rs` (deleted with `giap-draft` on 2026-09-10; the
>   decision rule lives on as `is_draft_decision_permitted`, which `POST /proposals/{id}/decide` uses). `SecurityPolicy::allow` does still return
>   `Ok(true)` unconditionally — both gates decide with pure rules in
>   `security/ports/policy.rs` and use the port for the audit trail, which is what `audit` mode is.
> - **"There is no encryption at rest" is half false.** `secrets.json` is an XChaCha20-Poly1305
>   envelope since PAI-2 P4. Both SQLite databases are still plaintext, and that is a recorded
>   deferral rather than an oversight.
> - **The four `api_key_*` fields are off `Settings`** since PAI-2 P2 and live in `SecretRepository`;
>   a build-breaking guard rejects any new secret-shaped field. `GET /settings` no longer serialises
>   a key because there is no longer a key on the struct.

### 1.3 There is no initiative

- The event bus is a closed three-variant enum — `Sensor | Camera | Device`
  (`shared/ports/event_bus.rs:29-33`) — with two consumers: the rules engine and the event-log
  bridge.
- Every production notification producer calls `broadcast()`: `routes.rs:542`,
  `pond-mcp-server/src/system.rs:214`, `schedule_executors.rs:107`, `main.rs:2660`. The durable
  offline queue and the FCM relay are only reachable from the *targeted* `send()` path
  (`broadcast_notification_sender.rs:68`), so both are **built, tested and dormant**.
- Unprompted speech does not exist. Every `voice_output.speak()` is downstream of a user utterance
  (`shared/services/chat.rs:933,953,1127,1593,1604`) or an explicit `/tts` request
  (`routes.rs:6395`).

### 1.4 There is no honest accounting

- ~~Four different resolution orders answer "how big is the context window", worst of them the live
  trimmer reading `std::env::var("GOOSE_CONTEXT_LIMIT")` with a hardcoded 8192 fallback.~~
  **FIXED 2026-08-04 by PAI-3 P1.** `ContextGovernor` owns the precedence, all four sites are
  repointed, and `no_budget_path_reads_the_context_limit_from_the_environment` keeps the env read
  from coming back.
- ~~Token estimation is `len/4 + 4`, corrected one turn late.~~ **PARTLY FIXED by PAI-3 P2.** A
  `TokenCounter` port now carries it, with a tiktoken-backed adapter on the live path and chars/4 as
  the declared fallback. The "corrected one turn late" half stands and is now load-bearing on
  purpose: no reachable counter is *exact* for a GGUF model.
- ~~`ModelRecord.context_length` exists but nothing feeds it.~~ **PARTLY FIXED 2026-08-06 by PAI-3
  P3.** The earlier correction — `gguf_record()` writes a real value, so it is not `None`
  everywhere — was true and one question short: `llamafile_record()` and `ollama_entry_to_record()`
  wrote `None`, and Ollama is the provider class rung 3 was designed for. Both now populate it
  (Ollama from `/api/show`'s `model_info`), the Gemma 4 rows were corrected from a copy-pasted 8192
  to the declared 131072, and rung 3 clamps a catalog value by the local ceiling because a declared
  maximum is not an allocation. ~~**`WindowSource::CatalogRecord` is still unreachable in
  production**~~ — **FIXED 2026-08-06 by PAI-3 P3b.** Both live `ContextInputs` sites now supply it:
  `routes.rs` through `state.model_repo`, and `GooseAdapter` through a `model_repo` threaded in for
  this (it had none, and `resolve_window` was static). `ModelStatusEntry.context_length` carries it
  to the Models UI, whose `CapabilityBadges` now prefers it over the frontend's own name heuristic.
  P3b also narrowed the rung: a catalog value is bounded by a lower `context_window_override`, and
  reports `WindowSource::Override` when that binds — otherwise populating the catalog would have
  overruled the hand-tuned KV cache the override exists for. Only the quarantined
  `pond-agent/src/agent.rs` still passes `None`.
- `reasoning_content` is parsed by llama.cpp and dropped: the identifier appears exactly once in
  `crates/`, in a comment (`pond-inference/src/provider.rs:422`).
- `AgentStreamEvent::Thinking` has **four consumers and zero producers**
  (`chat.rs:1084`, `routes.rs:1415`, `routes.rs:7180`, `main.rs:6178`).
- `ContextCompactor` — 277 lines of LLM summarisation — is stored on `ChatService` and never read
  (`chat.rs:8,195,197,270,424,425`); nothing anywhere calls `with_context_compactor`.

---

## 2. Sequencing

The eight are interdependent, but not symmetrically. Two are substrate everything personal stands
on, two are accounting every token-hungry feature stands on, two are capability, and two are
product. The product themes land last not because they matter least — they are the entire value —
but because shipping a proactive agent that cannot tell household members apart, or an ingest
pipeline with no redaction, would be worse than shipping neither.

```
PAI-1 Identity ──┬──────────────────────────────────────────┐
                 │                                          │
PAI-2 Guardrails ┴──────────────────────┐                   │
                                        │                   │
PAI-3 Context governor ── PAI-4 Compaction ── PAI-5 Thinking │
                                        │                   │
                                        └── PAI-6 Orchestration ──┬── PAI-7 Proactivity
                                                                  │            │
                                                                  └────────────┴── PAI-8 Ingest
```

| Doc | Workstream | Requirement | Depends on | Status |
|---|---|---|---|---|
| [01](./pai/01-identity-and-profile-boundaries.md) | Identity and profile boundaries | Hard profile boundaries | — | **P1-P10 LANDED.** VERIFIED on the Orin 2026-08-12. Open outside the phase list: `get_session_messages` and `get_session_attachment` take no `Principal`; the `giap-memory` MCP tools search at `ProfileScope::Household`; `language` is collected at onboarding and never written to `profiles.preferences` |
| [02](./pai/02-privacy-and-security-guardrails.md) | Privacy and security guardrails | Privacy/security guardrails | 01 | **P0-P5, P6a, P7, P8a LANDED; P6b partial; P8b blocked.** VERIFIED on the Orin 2026-08-12. `UNGATED_SENDERS` is empty and `MAX_UNGATED` is 0. P6b's outbound-connector-action gate is open, and the `giap-draft` staging gate was deleted on 2026-09-10; P8b waits on `Principal.proven_profile_id`, which is `None` on every path |
| [03](./pai/03-context-governor.md) | Context governor | Large context, used fully | — | **P1-P6 and 4b LANDED.** VERIFIED on the Orin 2026-08-12, before goose took back trimming and compaction of the live conversation (2026-09-11) and before the `device_budget` move (2026-09-24); the re-run is owed |
| [04](./pai/04-smart-compaction.md) | Smart compaction | Smart compaction | 03 | **Mostly superseded on 2026-09-11, when goose took back the live conversation.** P5 (prefix-cache state), P7a (the compact route, now calling goose) and P7b (`ContextPressureNote`) stand; P6's job is `run_quiet_compaction`; P1 is inert and P3 runs only on replay; P2 and P4 were deleted. Per-phase table at the top of the document's section 4 |
| [05](./pai/05-reasoning-and-thinking.md) | Reasoning and thinking | Ability to think | 03, 04 | **P1-P4, P6-P8 LANDED; P5 partial** (its reserve reaches only replay since 2026-09-11). VERIFIED on the Orin 2026-08-12; P6's reload replay verified on the Mac only |
| [06](./pai/06-multi-agent-orchestration.md) | Multi-agent orchestration | Multi-agent orchestration | 01, 02, 03, 04 | **P1-P3, P5-P11, P14-P17, P19 LANDED; P4 and P12 partial; P13 open; P18 deferred** (P4's history shrink reaches only replay since 2026-09-11; P12 lacks the approval path). VERIFIED on the Orin 2026-10-28: a four-step long task ran end to end, and every agent's prefill compiled into its own snapshot. `delegate` was PARTIALLY VERIFIED 2026-08-12 (a 2B declines to call it). `ext_orchestrator_enabled` ships off |
| [07](./pai/07-proactive-intelligence.md) | Proactive intelligence | Proactive not reactive | 01, 06 | **P1-P7 LANDED; P8 partial** (`TaskKind::ToolCall` never added). NOT VERIFIED on the Orin: the reviewer runs and yields no proposal, because a 2B omits the required `trigger_kind`. `proactive_review_enabled` ships off |
| [08](./pai/08-personal-context-streaming.md) | Personal context streaming | Personal context streaming | 01, 02, 07 | **P1 and P5 LANDED** (P5: read-only IMAP and CalDAV, 2026-08-17/18); **P2 partial** (tools only; `ext_context_enabled` ships off); **P3, P4, P6, P7, P8 not started.** VERIFIED on the Orin 2026-08-12 for P1. The cross-corpus index is [personal-context-index.md](./personal-context-index.md) |

`DESIGNED` means the document exists and its current-state claims are verified. Phases inside each
document carry their own `LANDED` / `DEFERRED` stamps as work completes, matching the convention in
`context-and-reasoning-roadmap.md`.

### Why this order and not another

- **01 before everything personal.** Memory, drafts, notifications and ingested items all need an
  owner. Retrofitting a `profile_id` through them later means a migration per table plus a
  backfill with no ground truth about who said what.
- **02 before 08.** Ingesting a mailbox into a store with no redaction, no encryption for
  connector tokens, and a policy layer that returns `Ok(true)` would be the single largest privacy
  regression in the product's history.
- **03 before 04, 05 and 06.** Compaction decisions, thinking budgets and per-subagent context
  allocations are all arithmetic on a number that four code paths currently disagree about.
- **04 before 06.** A subagent is a second context window opening on the same device. Without
  cache-age-aware compaction, delegation turns every parent turn into a full re-prefill.
- **06 before 07.** The proactive reasoner is a background agent with its own budget, its own tool
  scope and its own cancellation semantics. That is exactly what 06 builds.
- **07 before 08.** Ingest without a proposer is a database nobody reads. The proactive layer is
  what converts a stream of e-mail and calendar rows into something the household notices.

---

## 3. Cross-cutting invariants

These hold across all eight workstreams. A design that violates one is wrong regardless of how
good it looks in isolation.

1. **The KV prefix is sacred.** The system prefix is the KV prefix, and moving it costs a full
   re-prefill (measured at 3.7 s on the Orin for a 78-character delta).
   Anything session-specific or turn-specific rides the user message's `<system-context>` block,
   never the system prompt. This is why `system_prefix` and `extension_appendix` are global by
   design (`context-and-reasoning-roadmap.md`, Phase D1).
2. **Nothing blocks a turn on an LLM call.** Compaction, summarisation, consolidation and
   proactive reasoning all run in idle time and are cancelled by a new turn. A user-visible stall
   at 20 tok/s is a bug, not a trade-off.
3. **Deny by default, widen explicitly.** Every failure path in tool selection already widens to
   all tools; every failure path in *access* must narrow to none. Where those two conflict, access
   wins.
4. **Egress is tracked at the adapter, and now gated there too.** Any crate that reaches the
   network with its own `reqwest::Client` goes through `egress::begin_for` / `begin_as` and `EgressCall::finish`
   (PAI-2 P5), which checks `network_mode` before the socket opens and records the call either way.
   Verified 2026-08-05: `crates/pond-core/tests/egress_guard.rs` fails the build for an HTTP-sending
   source file that is in none of its three classification lists. The old wording — "copy
   `traced_send` from `pond-adapters-weather/src/lib.rs:15-30`" — was wrong twice over: the line
   range had rotted by nine lines, and copying a helper is not a guarantee.
5. **The domain owns policy; adapters own mechanism.** Anything that would have to be rewritten if
   Goose were swapped out belongs in `pond-core` behind a port.
6. **Settings fields are dispositioned or the build fails.**
   `every_settings_field_is_dispositioned` (`pond-core/src/user_data/domain/settings.rs`) must keep passing; new fields
   are consciously classified as UI-wired or headless.
7. **On-device reality bounds ambition.** One GPU, one resident model, ~102 GB/s of memory
   bandwidth. Parallelism that serialises on hardware is complexity without benefit; say so rather
   than shipping it.

---

## 4. Documentation debt corrected alongside this programme

Several existing documents now assert things the code contradicts. Accuracy is what makes these
documents worth keeping, so they are corrected as part of the workstream that touches them.

All of these are now **done**. Kept as a record of what was corrected and why.

| Document | Claim | Reality | Status |
|---|---|---|---|
| `architecture/token_tracking.md` ("Estimation") | "Real token counts are not available from Goose's `AgentEvent` stream" | Migration `0029_message_token_counts` and `TurnStats` landed; they are | FIXED; rewritten again by PAI-3 P6, whose rewrite moves the old line 28 |
| `architecture/scheduling.md:38` | `TaskKind` has two variants; seven MCP tools | Three variants (`SensorTrigger`); twelve tools | FIXED |
| `api.md:219` | "Token validation is currently a stub" | `SqliteHandshakeAdapter::validate_token` is real | FIXED |
| `architecture/model_capabilities.md` (`ModelCapabilities` struct) | `ModelCapabilities` has five fields | Six — `tool_calling` was missing from the doc | FIXED; the detection table below it still omitted the column until PAI-3 P6 |
| `security/ports/policy.rs:6-11` | Describes the unconditional loopback bypass "at lines 206-214" | Removed in #94; now gated behind `POND_DEV_ALLOW_LOOPBACK` | FIXED |
| `AGENTS.md` | "14 `giap-*` extensions", "57 tools" | **15** and **61**. I twice got this wrong before counting properly: `giap-toolkit` registers via the `TOOLKIT_EXTENSION` const, so a grep for `"giap-*"` string literals misses it. Count `register_builtin_extension(` call sites instead | FIXED 2026-08-04 |

---

## 5. What this programme deliberately does not do

- **It does not replace Goose.** `pond-agent` + `pond-inference` remain the quarantined
  independent path. Every design here works through the live `GooseAdapter` and keeps its domain
  logic in `pond-core` so a future swap stays possible.
- **It does not add cloud inference.** `cloud_fallback_enabled` stays off by default and no
  workstream here depends on it. PAI-8 ingests from cloud accounts; it never sends prompts to them.
- **It does not promise full-database encryption.** PAI-2 encrypts the high-sensitivity stores and
  writes down the SQLCipher migration path with its cost, rather than claiming a property the
  build system cannot currently deliver on a Jetson cross-build. As of P4 (2026-08-05) that means
  `secrets.json` only: both SQLite databases are still plaintext, and by default the key sits in
  the same directory as the ciphertext, so this protects a copied file rather than a stolen board.
- **It does not promise first-party WhatsApp.** PAI-8 is explicit about which messaging sources
  have a sane official read API and which require a bridge the user runs themselves.

---

## 6. Next, after the eight — measured work the programme uncovered but did not schedule

None of these is a PAI phase. They are the follow-ups the on-device measurements of 2026-08-11
turned up, recorded here rather than in a scratch file because each has a number attached and the
numbers are the argument. Full workings in
[`docs/developer/orin-prefill-measurement.md`](../developer/orin-prefill-measurement.md).

### 6.1 Warm the preamble at boot — the first turn is the only cold one left

**The finding that makes this worth doing: the KV prefix is shared across SESSIONS, not just across
turns.** Measured on an M4 with three interleaved turns — session A cold, then a brand-new session
B, then back to A:

| | plan | TTFT | prefill |
|---|---|---|---|
| A, turn 1 | `CreateContext` | 12 260 ms | 11 617 ms |
| **B, turn 1 (new session)** | **`ReusePrefix(6953)`** | **154 ms** | **66 ms** |
| A, turn 2 | `ReusePrefix(6845)` | 437 ms | 62 ms |

A brand-new conversation reused 6 953 tokens on its *first* turn, because the system prompt and tool
schemas sit at the front of every prompt and `ReusePrefix` matches on common prefix rather than on
session identity. So ~6 900 tokens of preamble are shared by every conversation on the pond — desktop,
voice, GOTG, every household member — and only each session's own tail is ever prefilled.

**Which means exactly one turn on the whole pond is cold: the first one after the process starts.**
4.8 s of model load plus 11.6 s of prefill on the Mac; the Orin's standalone prefill puts the same
preamble nearer 8 s there. That is the moment a home assistant feels broken, and it recurs on every
restart, deploy and reboot.

**Shipped 2026-08-27 (`c697626b`):** the `Agent` port's `prewarm`, implemented by `GooseAdapter`,
prefills the rendered preamble before the first message. The rest of this section is the case as it
was made.

Prefilling the shared preamble once at startup would make the first user turn `ReusePrefix` instead
of `CreateContext`. It composes with 6.2: fewer tools means a smaller warm-up and a smaller preamble.

**The honest cost, which needs measuring rather than assuming:** it spends one full prefill of GPU
time at boot whether or not anybody speaks, and on a device that restarts often that may not pay. It
also wants care about *what* is warmed — warming a preamble that the first real turn then diverges
from buys nothing, so the warm-up prompt has to be the real rendered preamble, not an approximation
of it.

### 6.2 `tool_selection_mode = "relevant"` needs burn-in, and the win is 3x

Same machine, model and question, `"all"` against `"relevant"`: 61 tools to 17, prompt 6 969 tokens
to 2 495, prefill 11 690 ms to 3 326 ms, **TTFT 12 344 ms to 4 065 ms**. The model still reasoned
(122 tokens against 128) and still answered, because what was cut is schema and not thinking. Tool
schemas are **88 %** of a default turn's prompt — 25 449 chars of JSON for 61 tools against 2 548
chars of system prompt.

It is compatible with the prefix cache: selection resolves **once per session from the first
message**, then caches and persists, so the tool set is stable within a session and turn 2 still
takes `ReusePrefix` (verified — 17 tools on both turns, prefill 3 258 ms then 27 ms).

**Status (2026-10-23):** `tool_selection_mode` still defaults to `all`
(`Settings::default_tool_selection_mode`); a third mode, `minimal`, now exists beside `relevant`.

**The reason it is not already the default is a real objection, and sharper than "it sometimes
mis-scores".** The set is chosen from the FIRST message, i.e. at the least informed moment of the
conversation, and the failure is silent: a session that opens with "what's the weather?" may never be
offered device control, and the model does not announce a missing tool — it simply does not act.
Burn-in means measuring that miss rate on real conversations, not deciding it is unlikely.

### 6.3 The 22 % decode gap

GIAP decodes at 38.9-40.4 tok/s where `llama-bench` does 50.6 on the same box and model. Decode is
per-token, so prompt size does not explain it. Worth checking in this order: the thought filter
running per token, SSE frame construction per token, sampler configuration differences, and PAI-5
P2's reasoning-token accounting. It is roughly 10 tok/s on every answer the pond ever gives, which
compounds differently from the prefill wins above — those help the first token, this helps all of
them.

### 6.4 Re-run the on-device numbers through the deployed binary

The Orin sweep used the standalone `llama.cpp`, not the tree `goose-local-inference` links, so its
prefill figures characterise the hardware rather than the engine. The Mac run closed that gap
locally and showed no degradation; the Jetson equivalent still wants the branch deployed there, and
the same run would also settle PAI-4 P5's second clause on the hardware that matters.

