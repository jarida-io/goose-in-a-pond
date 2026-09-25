# PAI-4 — Smart compaction: model, time, and cache age

Requirement: *smart compaction based on different models, time and cache age.* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: [PAI-3](./03-context-governor.md) — compaction is arithmetic on the window.

Verified against code 2026-08-03.

---

## 1. What is true today

### 1.1 Hybrid compaction works and stays

`hybrid_compaction_enabled` defaults **true** (`settings.rs:425-434`). Two halves:

- **The deterministic trimmer** — `turn_trimmer.rs`, "the hard-real-time half… Never calls a model,
  never blocks" (`:1-31`). Strips stale `<system-context>` blocks from prior user messages,
  truncates oversized tool results head-and-tail, splices the rolling summary at the front, and
  drops the **oldest complete turns** until the estimate fits. A turn is a user message plus
  everything until the next one (`last_turn_start`, `:95-100`), so tool results never orphan.
- **The idle rolling summary** — `SessionSummaryService` (`shared/services/session_summary.rs`).
  Runs after `summary_idle_secs`, never at startup, cancelled by a new turn.
  `KEEP_RECENT_MESSAGES = 6`, `MIN_UNSUMMARIZED_MESSAGES = 4`. Result lands in
  `sessions.rolling_summary` (migration `0030`) and the trimmer splices it as
  `<conversation-summary>`. **Turns read it; they never wait for it.**

GIAP also disables Goose's competing machinery on this path, in `goose_env_knobs`. Note the two
knobs are gated differently, which I originally ran together: `GOOSE_AUTO_COMPACT_THRESHOLD = "1.0"`
is set for **every** provider whenever `hybrid_compaction_enabled` is on, while
`GOOSE_TOOL_PAIR_SUMMARIZATION = "false"` is `local`/`gguf` only — the code's reason being that "an
HTTP provider's spare capacity is not ours to save".

### 1.2 Compaction is model-tier-aware and nothing else

`CompactionProfile::from_context_window` varies *budgets* by window size. It does not vary
*strategy*, and nothing anywhere considers how old a turn is or what state the KV cache is in.

> **Half-closed 2026-08-06.** The model axis is done: P1 added `ModelClass`, and P2 gave it a
> mechanism to gate — the large tier's re-summarisation, which the on-device tiers do not run. So
> strategy now varies by tier, and `run_compaction_pass` is where. The time axis is done as far as
> this programme intends to take it: P4's compact-on-resume, and P3's age weighting, which since
> 2026-08-06 gives the trimmer a tool-result rung keyed on `compaction_verbatim_days`. **Cache age is
> still entirely unmodelled** — that is P5, and the sentence above remains true of it word for word.
>
> **Cache age closed in code 2026-08-06 by P5, and not yet in evidence.** `prefix_cache.rs` supplies
> the state, the adapter records all six invalidation reasons, and the trimmer's age rung now also
> fires on a cold prefix. So the heading's claim is retired as a description of the code. It is *not*
> retired as a description of what is known: section 7's warm-versus-cold TTFT measurement is the
> phase's acceptance criterion and has not been taken. Read the P5 stamp in section 4 before treating
> the cache axis as settled.

### 1.3 `ContextCompactor` is dead code

277 lines of LLM summarisation (`context/context_compactor.rs`): `needs_compaction`, `compact`,
`summarise`, an `[Earlier conversation summary]` splice, and a `trim_to_budget` fallback on LLM
error. It is stored on `ChatService` as `compactor: Option<ContextCompactor>` (`chat.rs:197`),
initialised `None` (`:270`), settable via `with_context_compactor` (`:424-425`) — and **never
read**. No caller of `with_context_compactor` exists anywhere in `crates/`.

> **Deleted 2026-08-06, and the deletion is correct.** `context_compactor.rs` and
> `compact_encoding.rs` are gone and no reference to `ContextCompactor` remains anywhere in
> `crates/`. This section is kept because the *diagnosis* was right and the phase list was built on
> it; only the disposition changed, from "revive" to "rewrite".
>
> The reason is PAI-3, not tidiness. The deleted implementation was built on
> `USABLE_HISTORY_CHARS` and its own `const CHARS_PER_TOKEN: usize = 4` — precisely the estimator
> PAI-3 P2 spent a phase replacing with the `TokenCounter` port, and precisely the kind of second
> copy PAI-3 P1 exists to prevent. Reviving it would have re-imported the heuristic two landed
> phases removed, into the one tier where an accurate count is affordable. Dead code that has
> drifted three phases behind the live path is not an asset waiting to be switched on.

### 1.4 `should_compact` is computed and ignored — CLOSED by P6, 2026-08-06

`ContextMonitor::check_context_health` returns `should_compact` when utilisation exceeds 75% or fewer
than three turns remain at the current growth rate. The API emitted a `context_warning` SSE event and
logged it, and that was the whole response: **nothing acted on it server-side**, and `reset_session`
was never called from any handler — so the growth map also only ever grew.

P6 closed both halves. `claim_compaction` is what the server now acts on, `spawn_pressure_compaction`
in `routes.rs` is where, and `reset_session` is called from `delete_session`. The `context_warning`
frame is unchanged. See the P6 stamp in section 4 for what "act" turned out to mean and for the one
place the design bullet could not be followed literally.

### 1.5 The cost of moving the prefix is known and unmodelled

The repository already measured it: a 78-character system-prompt delta cost every session a full
re-prefill on its second turn — 3.7 s on the Orin (`goose_agent.rs:1000-1011`). Phase D made tool
selection *sticky* per session for exactly this reason. But the compaction path has no notion of
the prefix's state at all; it decides purely on token arithmetic.

Also relevant: any turn containing an image bypasses KV retention entirely, which is why
`MAX_HISTORY_REPLAY_IMAGES = 1` (`context/image_history.rs`).

---

## 2. The gap

Compaction today asks one question — "does it fit?" — and answers it with one strategy. The
requirement names three axes it should also consider, and each corresponds to a real cost the
current design pays blindly:

- **Model** — a 20 tok/s on-device model and a hosted 128K model should not compact the same way.
- **Time** — a turn from three days ago is not worth the same tokens as one from five minutes ago,
  and a session resumed after a gap is a free opportunity nobody takes.
- **Cache age** — compaction that moves the prefix costs seconds of re-prefill; compaction performed
  when the cache is *already* cold costs nothing.

---

## 3. Design

### 3.1 Model axis — vary strategy, not just budgets

| Model class | Strategy |
|---|---|
| Small on-device (`local`/`gguf`, ≤ 12K window) | Deterministic trim, plus the idle rolling summary. No **blocking** LLM in the compaction path — a summarisation stall at 20 tok/s is a user-visible hang, which is precisely why hybrid compaction exists. |
| Medium (32K, Ollama/llamafile) | Deterministic trim + idle rolling summary (today's behaviour). |
| Large / HTTP (≥ 64K) | Deterministic trim + idle summary + **LLM re-summarisation of the summary itself** when it grows stale. Here the summarisation call is cheap and off the critical path. |

> **Corrected 2026-08-06 by P1, in two places.**
>
> **The small row said "deterministic trim only. No LLM in the compaction path."** The stated reason
> is a stall at 20 tok/s, and the idle rolling summary cannot produce one: it runs only after
> `summary_idle_secs`, a new turn cancels it, and turns read `sessions.rolling_summary` without ever
> awaiting it (1.1; invariant 5). Reading the row literally would have removed the rolling summary
> from the device whose window runs out first, to prevent a hang that path structurally cannot cause.
> What the small tier really excludes is a model call the *compaction* has to wait for — which is the
> large tier's re-summarisation, and nothing else here. The idle summary does carry a real on-device
> cost, but a different one: it evicts the prefix cache, so the next turn pays a prefill it would not
> have. That is P5's arithmetic, it applies to the medium tier just as much, and it needs a
> measurement rather than a flag flipped here.
>
> **The three rows each mix a window size with a provider**, and two live configurations fall between
> them: an 8K *hosted* model, and a 131,072-token *Ollama* model on the Orin, which PAI-3 P3 made
> reachable by teaching `OllamaCatalogProvider` to read `context_length` from `/api/show`.
> `model_class.rs` implements the table as the two independent questions it is made of — a window
> bracket (`SMALL_WINDOW_CEILING` / `LARGE_WINDOW_FLOOR`) and an on-device provider set. Every row
> above is reproduced; the small hosted model takes the small strategy (at 12K there is no span worth
> re-summarising), and the 131K Ollama model takes the *medium* strategy with the *large* budgets,
> because the window is generous and the compute is not.

The large-model tier is where re-summarisation belongs. It is a problem the on-device tier does not
have, and the answer to why nothing was ever wired: nobody had a tier where it was safe.

> **Corrected 2026-08-06.** This paragraph said the dead `ContextCompactor` would be "revived rather
> than deleted". It has since been deleted, correctly — see 1.3. What this tier needs is written
> fresh against the landed governor: `TokenCounter` for the measurement, `CompactionProfile`'s
> interpolated curve for the budget, and `ModelClass` for the gate. The salvage from the old file is
> its *shape*, which was sound and is worth restating so it is not rediscovered: summarise the older
> span, splice the result back as a single message, keep the most recent turns verbatim, and fall
> back to a deterministic trim whenever the LLM call fails so a chat turn is never interrupted by
> compaction.
>
> **LANDED 2026-08-06 by P2**, as `context/resummarisation.rs` (the gate) plus
> `SessionSummaryService::resummarise` (the mechanism), called from `run_compaction_pass`. Read the
> P2 stamp in section 4 before changing anything here. Two clauses above did not survive contact
> with the code: "re-summarisation of the summary itself" is implemented as rebuilding from the
> **source messages** rather than from the summary chain — re-summarising the chain is what `refresh`
> already does, and doing it again buys nothing — and "fall back to a deterministic trim" is
> structural rather than a branch, because this pass is not on a turn's path and the trimmer runs
> either way.

### 3.2 Time axis — age-weighted retention and compact-on-resume

**Age-weighted retention.** The trimmer currently treats every retained turn as equally valuable and
drops strictly oldest-first. Add recency tiers:

| Age | Treatment |
|---|---|
| Current session, recent turns | Verbatim |
| Same day | Eligible for tool-result truncation and `<system-context>` stripping first |
| Older than `compaction_verbatim_days` | Represented by the rolling summary only |

This is not a new mechanism — it is a priority order over mechanisms the trimmer already has. It
matters most for the long-lived "household" sessions PAI-7 will create, which are never closed.

> **Corrected 2026-08-06 by P3, on rows one and three.** Row three ("represented by the rolling
> summary only") cannot be honoured by the trimmer and was not implemented: the summary's coverage is
> a through-pointer into `session_messages` while the trimmer addresses the engine conversation by
> position, and the summariser leaves the newest six messages uncovered by construction — so
> "the summary represents it" is unverifiable in general and false for the tail. Dropping on that
> premise would lose messages silently. Aged material is *degraded* instead, at
> `AGED_TOOL_RESULT_MAX_CHARS`. Row one turned out to be the load-bearing row and is enforced
> literally: everything from the last turn's start is spared, which is what keeps a session reopened
> after a week from degrading the very message the model is about to answer. Row two's implied
> reordering is a no-op and was not built — age is monotonic with position, so the existing
> oldest-first drop already is most-aged-first.

**Compact-on-resume — the highest-value time behaviour, and it is free.** A session reopened after a
gap is about to pay a full prefill anyway (the KV cache is long gone). Today compaction happens
*during* the first turn back, while the user waits on a token stream. Doing it on resume, before the
first user message arrives, costs the user nothing:

```
session resumed && idle_gap > resume_compaction_idle_secs
    → run full compaction (and, on large models, re-summarisation) before the first turn
```

The trigger reuses the pattern already proven in `user_data/services/consolidation_schedule.rs` —
a pure `should_run` gate with a startup guard — rather than inventing new scheduling.

> **Corrected 2026-08-06 by P4, on what "run full compaction" can mean today.** The pseudo-code
> reads as though a single compaction pass exists to be invoked. It does not: hybrid compaction is
> two halves with opposite timing. The deterministic trimmer is a function of the turn being
> assembled, so there is nothing to pre-run and nothing to save — it is cheap by construction. The
> half worth moving off the critical path is the rolling summary, and there the gap is real: the
> idle refresh loop skips any session whose `updated_at` predates process start, so a conversation
> from before the last restart keeps a summary frozen at the restart and the trimmer splices that
> stale summary into every turn until four new messages accumulate. Compact-on-resume refreshes it
> at the reopen. The large tier's re-summarisation is the third thing this trigger runs, and P2
> landed it on 2026-08-06.
>
> **`resume` is a user action, not an elapsed duration.** The startup guard the design points at has
> a specific shape here that the one-line rule hides: at boot every stored session satisfies
> `idle_gap > resume_compaction_idle_secs`, so a gate keyed on the gap alone would compact the whole
> history store on startup. `ResumeGateInputs::reopened` is the guard, and it can only be set by a
> request somebody made.

### 3.3 Cache-age axis — the core insight

Introduce prefix-cache state as an *input* to the compaction decision:

```rust
pub struct PrefixCacheState {
    pub built_at: Instant,
    pub hash: u64,
    pub turns_served: u32,
    pub invalidated_by: Option<InvalidationReason>,
    // ProviderRebuilt | ModelSwapped | SessionResumed | MultimodalTurn |
    // ToolSetChanged | PromptChanged
}
```

The rule:

> **Recompact aggressively when the cache is already lost; otherwise prefer edits that keep the
> prefix byte-identical.**

Because the prefix is *already* gone after a provider rebuild, a model swap, a session resume, or a
multimodal turn, a full recompaction at that moment is genuinely free — the re-prefill is happening
regardless. Conversely, on a warm cache mid-conversation the trimmer should prefer operations that
touch only the tail-adjacent middle (tool-result truncation, dropping a whole old turn) over anything
that perturbs the front.

This also explains an existing accepted cost more precisely: Phase D noted that two sessions
alternating on one model diverge at the tools block and re-prefill more. With `PrefixCacheState`,
that alternation becomes *observable* — `invalidated_by: ToolSetChanged` — and therefore an
opportunity to recompact rather than an invisible tax.

`turns_served` is the value signal: a prefix that has served 30 turns is worth protecting; one that
has served two is not.

### 3.4 Act on `should_compact`, between turns

`ContextHealth.should_compact` becomes actionable — but compaction runs **between** turns, never
during one, consistent with **invariant 1** (corrected 2026-08-06 by P6: this line said "invariant
2", which is the tool-result rule; the one meant is "compaction never blocks a turn, ever"). The
existing `context_warning` SSE keeps
flowing to the client. `ContextMonitor::reset_session` gets called when a session is cleared or its
history is compacted, so the growth-rate samples stop describing a conversation that no longer
exists.

> **Corrected 2026-08-06 by P6, in two places.**
>
> *A standing condition is not an event.* This section treats `should_compact` as something that
> happens; it is something that becomes and stays true. Utilisation is monotone across a
> conversation, so once a session crosses 75% the predicate is true on every subsequent turn, and
> "act on it" with no rate limit means a summarisation between every pair of turns. The rate limiter
> is `COMPACTION_COOLDOWN_TURNS`, and it is not a detail of the implementation — without it this
> section describes the exact failure P4 spent `MIN_RESUME_IDLE_SECS` avoiding on the time axis.
>
> *`reset_session` after compaction would undo the rate limiter.* "Cleared **or** compacted" reads as
> one action for two situations, but they are not the same situation. A cleared session is gone: drop
> everything. A compacted session is still here and its window did **not** shrink — the rolling
> summary refresh changes what the trimmer splices, not what the engine reports next turn — so a full
> reset would restore 0% utilisation, an empty growth window and, fatally, no cooldown stamp, and the
> pass would re-fire on the very next turn. `note_compacted` is the post-compaction call: it drops
> the growth samples, which genuinely described a differently-shaped history, and leaves utilisation
> and the cooldown alone.

### 3.5 An explicit compaction endpoint

`POST /api/v1/sessions/{id}/compact` — no such route exists today. Returns what changed: turns
dropped, tokens reclaimed, whether the summary was refreshed, and whether the prefix survived. Users
who understand their assistant's memory pressure should be able to act on it, and it makes every
phase above manually testable.

### 3.6 What is not changing

The trimmer stays deterministic and non-blocking. Tool results keep travelling with their turn. The
rolling summary stays idle-only and cancellable. Images stay out of the trimmer
(`turn_trimmer.rs:18-30` explains why, and adding a second image rule there would drift from
`image_history.rs`).

---

## 4. Phases

- **P1 — LANDED 2026-08-06, as two axes rather than three rows.**
  `models/services/context/model_class.rs`: `ModelClass { Small | Medium | Large }`,
  `CompactionStrategy { deterministic_trim, idle_rolling_summary, llm_resummarisation }`,
  `ModelClass::classify(provider, window)` and `ModelClass::from_resolution(provider,
  &WindowResolution)`. Domain only — no call site yet, same shape as PAI-3 P1, and **P2 is the first
  consumer**. 15 unit tests.

  **The table in 3.1 could not be implemented as written, and the reason is a real gap rather than a
  wording quibble.** Each of its three rows names a window size *and* a provider, so two
  configurations that exist today fall between them: an 8K hosted model, and a 131,072-token Ollama
  model on the Orin — which PAI-3 P3 made genuinely reachable when it taught `OllamaCatalogProvider`
  to read `context_length` from `/api/show`. The implementation splits the rows into the two
  independent questions they are made of, and both cells then have answers. The window brackets are
  `SMALL_WINDOW_CEILING = 12_288` and `LARGE_WINDOW_FLOOR = 65_536`, which are not new numbers:
  12,288 is `use_compact_prompt`'s step and `PROFILE_ANCHORS`' third point, 65,536 is its fifth.

  **`runs_on_this_device` is deliberately NOT `ContextGovernor::is_local_provider`, and conflating
  them would be the defect this phase exists to avoid.** The governor's predicate answers "is the
  preamble re-prefilled here every turn?", which is true for the in-process engine and false for
  Ollama. This one answers "would an extra model call compete with the turn the user is waiting
  on?", which is true for Ollama and llamafile as well — on the Orin they are HTTP to `127.0.0.1`
  and the tokens come off the same 102 GB/s. So `ON_DEVICE_PROVIDERS` is
  `local | gguf | ollama | llamafile`, matched case-insensitively, and it is a **deny-list for the
  large tier**: it is safe when too wide and unsafe when too narrow, which is why a provider that
  *could* point at another machine (`OLLAMA_HOST`) is kept inside it. Only `Large` unlocks a
  mechanism, so every fallback resolves downward.

  **Where this contradicts the document, and the contradiction is deliberate.** 3.1's small-tier row
  and invariant 3 both say no LLM in the compaction path on-device. `strategy()` nevertheless
  reports `idle_rolling_summary: true` for `Small`. The stated reason for the prohibition — "a
  summarisation stall at 20 tok/s is a user-visible hang" — is a hazard the idle rolling summary
  structurally cannot produce: it runs only after `summary_idle_secs`, it is cancelled by a new
  turn, and a turn reads `sessions.rolling_summary` without ever awaiting it (1.1, and invariant 5
  says the same). Setting the flag false would have removed the rolling summary from the one device
  whose window runs out first, to prevent a stall that path cannot cause — and the phase text is
  explicit that P1 preserves today's behaviour on the small and medium tiers. There *is* a real
  on-device cost to an idle summarisation: it evicts the prefix cache, so the next turn pays a
  prefill it would not have. That is cache-age arithmetic, it applies to the medium tier equally,
  and it belongs to **P5** with a measurement behind it. Both 3.1 and invariant 3 now carry that
  correction; `strategy()`'s doc comment carries the argument.

  **Consequence: `Small` and `Medium` select the same strategy today**, and a test asserts it rather
  than leaving it to be discovered as a bug. The classes are not redundant — they are the gate P3's
  `compaction_verbatim_days` and P5's cache rules key on — but in P1 the only tier whose strategy
  differs is `Large`.

  **What the compiler could not have caught.** `CompactionProfile` gained no field. `turn_trimmer.rs`
  constructs it with exhaustive literals in two test helpers, so a new field would have broken a file
  a concurrent session was expected to hold — the same structural collision that nearly destroyed the
  encrypted secret store in the PAI-2 batch. Keeping the profile's signature untouched is what let
  PAI-3 P4 land under the same conditions, and it is why the class is derived rather than stored.
- **P2 — LANDED 2026-08-06, respecified first, and nothing was revived.** The phase used to read
  "revive `ContextCompactor`". That file was deleted on 2026-08-06 and the deletion is right (see
  1.3): it carried its own `CHARS_PER_TOKEN = 4` and `USABLE_HISTORY_CHARS`, the estimator PAI-3 P2
  replaced with the `TokenCounter` port, so reviving it would have re-imported a heuristic two landed
  phases removed — into the one tier that can afford an accurate count. What shipped was written
  fresh against `TokenCounter`, `CompactionProfile` and `ModelClass`, and no line of the deleted file
  came back.

  `models/services/context/resummarisation.rs`: `ResummariseGateInputs`,
  `SkipReason { TierForbidsModelCall | NoSummaryYet | SpanStillFitsHistory | NoRoomToImprove |
  NothingCovered | SourceTooLargeToRead }`, `GateDecision`, `should_resummarise`,
  `resummary_budget_tokens`, `source_budget_tokens`, `newest_affordable_start`, `budget_as_words`.
  Fifteen unit tests, pure — no clock, no database, no model, the shape P4's `resume_compaction`
  proved. The mechanism is `SessionSummaryService::resummarise` in
  `shared/services/session_summary.rs`, with eight tests that drive it against a real storage adapter
  and a stub provider. The caller is `run_compaction_pass` in `routes.rs`, which is the shared body
  of both existing triggers — so P2 needed no third trigger of its own, and the hole P4 and P6 each
  left for it by name ("the large tier's re-summarisation is P2's, and this gate will call it when it
  exists") is the hole it fills.

  **What "re-summarisation of the summary itself" turned out to mean, which is not what it sounds
  like.** `refresh` is a *chain*: every pass feeds the model its own previous 3-5 sentences plus
  whatever is new. Detail lost on pass *n* cannot return on pass *n+1*, and the loss compounds
  silently. On the small and medium tiers that is the right trade, because the alternative costs a
  call on the box the next turn's prefill needs. On the large tier the source messages are still
  sitting in `session_messages` and the call is somebody else's hardware — so the mechanism is to
  **rebuild from the source span instead of from the chain**, with a budget the window can afford
  (`history_token_budget / 16`, clamped to 256..2,048; 1,250 tokens at the large tier's floor against
  the ~100 the chain produces). It is persisted under the **same through-pointer**: a rebuild changes
  how faithfully the covered span is represented, never which messages are claimed to be covered.

  **The gate is two conditions and neither is a magic number.** The covered span must have outgrown
  `history_token_budget` — below that the trimmer could still carry it verbatim, so the summary is a
  convenience rather than the only record of it, and the number is the trimmer's own. And the budget
  must allow more than a doubling of what the summary already spends, or the call would rewrite the
  same-sized artefact for nothing.

  **What I claimed and then had to withdraw, caught by my own test.** The first draft called the
  second condition "self-clearing" and asserted that a summary at *half* the budget closes the gate.
  It does not — at exactly half, a doubling is still available, which is the rule as written — and
  the test failed on the boundary. Worse, checking why exposed a real interaction the phase brief
  does not mention: `refresh` runs on every tier and asks for "3-5 sentences maximum", so as soon as
  four new messages accumulate past the pointer it folds the rebuilt summary back down and the gate
  opens again. The two mechanisms genuinely oscillate. That is not hidden damage — each firing
  restores fidelity the collapse destroyed — but the honest rate is "at most one rebuild per
  compaction pass", not "once per session", and the module says so instead of the thing I wanted to
  be true. **No new rate limiter was added**, deliberately: a pass only happens when
  `claim_compaction` grants one (P6, one per three turns) or a session is reopened past
  `resume_compaction_idle_secs` (P4), and a third limiter here would be a second copy of a rule those
  phases own. Reconciling the two budgets properly — teaching `refresh` that a large window can
  afford more than three sentences — touches the summary on a path P1 and P6 both argued about at
  length and needs P5's cache-age measurement. It is named, not done.

  **"Fall back to a deterministic trim on any LLM failure" could not be implemented as written, and
  saying so is better than a call nothing waits on.** At this seam the fallback is structural: the
  trimmer runs on every turn regardless of what happened here, and this pass is not on a turn's path
  at all. Every failure — a tier that forbids the call, no summary yet, a dangling through-pointer, a
  cancelled token, a provider error, a response no longer than what is stored — resolves to "leave
  the stored summary exactly as it was", which is the behaviour every tier had before this phase.

  **The gate applies to the re-summarisation and to nothing else.** `run_compaction_pass` still calls
  `refresh` unconditionally, on every tier, exactly as P6 left it. Extending
  `permits_compaction_model_call()` upward to cover the refresh would switch the rolling summary off
  on the small tier — which P1 argued against at length in 3.1 and invariant 3, and which P6
  explicitly declined to do.

  **And the first test that actually executes it** — the clause that survived the respec, because 277
  lines with no caller passed every gate this repo has for as long as they existed. Beyond driving
  the mechanism end to end, one test exists purely against that failure mode:
  `the_large_tier_is_reachable_through_the_rungs_an_off_turn_pass_can_supply`. An off-turn pass has
  no `engine_reported` (that arrives on `TurnStats`, during a turn) and no `registry_pinned` (that is
  the adapter's), so `Large` could have been unreachable in production while every other test passed
  — the `ProfileScope::Owner` shape, a fixture production cannot produce. It pins both live routes
  into the tier, a catalog row and the name-derived capability window, and pins that an Ollama model
  declaring 131,072 still is not cleared.

  **Not done, and named.** No live-server run, the same gap P4 and P6 both recorded: this changes a
  route side effect and adds a second background model call, which is what `scripts/live-test.sh`
  exists to catch. There is no test asserting that the `pond-api` wiring fires — `compaction_model_class`
  and `run_compaction_pass` are private to `routes.rs` and reaching them needs an `AppState`; what is
  pinned instead is that the tier they resolve is reachable and that the mechanism they call behaves.
  `note_compacted` still keys on `RefreshOutcome::Refreshed` only, deliberately: a rebuild changes the
  summary's size, not the message history the growth samples measured.

  > **Added 2026-08-06 by review: this mechanism does not fire on any configuration GIAP ships, and
  > the stamp above did not say so.** `ModelClass::Large` requires
  > `!runs_on_this_device(provider)`. Every provider `pond-desktop/src` offers is on-device —
  > `gguf`, `ollama`, `llamafile`, `local` — and `crates/pond-api/src/routes.rs` contains no hosted
  > provider anywhere. So `permits_compaction_model_call()` is false on every pond a user can
  > assemble through the product.
  >
  > It is not dead code, and the distinction matters. `Settings.chat_provider` is an unvalidated
  > `String`, and `provider_shim.rs` handles `openai`, `anthropic`, `google` and `databricks`
  > explicitly — so a hand-edited setting reaches the tier and the mechanism runs. What is true is
  > that **no shipped path reaches it**, which for a privacy-first local assistant is arguably
  > correct rather than a defect: the large tier exists for the hosted case 3.1 names, and GIAP does
  > not currently ship one.
  >
  > It is recorded because it is the same shape as the failure this phase was respecified to avoid,
  > arriving one level up. The original `ContextCompactor` was code with no caller. This is a caller
  > with no configuration — and `the_large_tier_is_reachable_through_the_rungs_an_off_turn_pass_can_supply`
  > guards precisely half of that question. It proves an off-turn pass can resolve a *window* into
  > `Large`, which was the sharp risk and is genuinely pinned; it says nothing about whether any
  > *provider* on a real pond clears the on-device test, and that is the half that turns out to
  > decide it. A reachability guard is only as good as the narrowest input it varies.
  >
  > What would make it fire: shipping a hosted-provider option, or a user setting `chat_provider`
  > by hand. Neither should be inferred from this stamp without checking, and the honest reading of
  > the phase today is "the mechanism is correct and untriggered".
- **P3 — LANDED 2026-08-06, as one rung rather than three tiers, because the trimmer cannot
  verify the claim the third tier rests on.** `turn_trimmer.rs` gained `DEFAULT_VERBATIM_DAYS = 3`,
  `AGED_TOOL_RESULT_MAX_CHARS` (a quarter of `TOOL_RESULT_MAX_CHARS`), `verbatim_horizon_from_days`,
  `TrimMessage::age_secs`, `TrimOutcome::aged_truncations`, and a seventh parameter on
  `trim_history`. Six new unit tests (`pond-core` 785 lib, up from 779). `compaction_verbatim_days`
  is a new headless setting whose default reads the trimmer's constant, so the setting's default and
  the code's cannot drift — the same construction P4 used. The caller is `trim_goose_history` in
  `pond-adapters-goose`, which fills `age_secs` from `goose::conversation::message::Message::created`
  and reads the horizon from a `last_verbatim_days` cache written on the settings path beside
  `last_window`.

  **The table in 3.2 asks for something the trimmer is not in a position to do, and shipping it
  would have lost messages silently.** Row three says material older than the horizon is
  "represented by the rolling summary only" — so it can be dropped. The trimmer cannot establish
  that premise. The summary's coverage is a through-pointer into `session_messages`
  (`SessionSummaryService`), the trimmer sees the *engine* conversation addressed by position, and
  nothing here maps between the two. Worse, the summariser leaves the newest
  `KEEP_RECENT_MESSAGES = 6` uncovered by construction, so for the tail the premise is not merely
  unverifiable but false. Dropping on it would have destroyed turns nothing had recorded, and every
  test would still have passed. What shipped instead degrades that material: an aged tool result is
  re-truncated head-and-tail at a quarter of the flat cap, which is a rung *between* "leave it
  alone" and "drop the whole turn" and loses strictly less than the drop it displaces.

  **Age changes what is sacrificed, not when.** The rung fires only when the conversation is already
  over budget. This is invariant 4, and it is not a detail: the edit lands at the FRONT of the
  conversation and invalidates the KV prefix exactly as dropping a turn would, so firing it on a
  conversation that already fits would spend a full re-prefill to save tokens nobody needed.
  Removing that gate makes `age_weighting_never_touches_a_conversation_that_already_fits` fail with
  *left: 1, right: 0*, and takes the idempotence test with it.

  **Row one turned out to be the load-bearing one.** "Current session, recent turns: verbatim" is
  enforced by sparing everything from `last_turn_start` onward, and it matters most in precisely the
  case age weighting exists for — a session reopened after a week, where *every* message is past the
  horizon including the one the model is about to answer. Dropping the guard makes
  `the_last_turn_is_verbatim_even_when_the_whole_session_is_aged` fail with *"exactly the tool result
  OUTSIDE the last turn may be degraded", left: 2, right: 1*.

  **Age is given no say in the drop ORDER, deliberately.** 3.2 opens by observing the trimmer "drops
  strictly oldest-first" as though that were the defect. It is not: age is monotonic with position in
  a conversation, so oldest-first already *is* most-aged-first, and re-deriving that order from
  timestamps would be a second implementation of the same ordering with a clock-skew failure mode the
  current one cannot have. Claiming a reordering here would have been a vacuous change.

  **Three narrowing defaults, each on a different axis.** `age_secs: None` (a caller that cannot
  establish an age) is treated as recent, never degraded. A future timestamp reads as age 0 via
  `saturating_sub`, matching `resume_compaction::idle_gap_since` — clock skew reads as "active",
  never as enormous age. And `compaction_verbatim_days = 0` is the only off switch; there is no
  separate boolean that could fall out of step with the number.

  **What is deferred, and named.** The hydration replay is NOT age-weighted: `plan_replay` takes
  `(role, text)` pairs, and threading timestamps through the adapter's pre-filtering — which assigns
  the indices `plan_replay` relies on — would widen that seam to buy a rung that only fires when a
  replay is over budget, where the drop loop already acts. It passes `None` rather than inventing an
  age. There is also no live-server run, the same gap P2, P4 and P6 all recorded.

  **Found while gating, not caused by this phase, and left alone deliberately.**
  `cargo test -p pond-adapters-goose` fails two tests at HEAD *before* this change:
  `the_adapter_reads_the_catalog_it_was_given` and
  `a_catalog_window_reaches_the_governor_from_the_adapter`, both asserting an Ollama model resolves
  to 131,072 — the behaviour f770f4de deliberately ended when rung 3 began clamping a catalog maximum
  for anything `runs_on_this_device`. They are the same "test asserting the old bug" class f770f4de
  found twice already, surviving here because `pond-adapters-goose` is outside the fast-crate set and
  CI only `cargo check`s it. Correcting another phase's assertions was not this phase's to do, but
  they are latent and named.
- **P4 — LANDED 2026-08-06, with a call site, and doing less than "full compaction" for a
  reason.** `models/services/context/resume_compaction.rs`: `ResumeGateInputs`,
  `SkipReason { Disabled | NotAReopen | NoPriorHistory | StillWarm | NoSummariser }`,
  `GateDecision`, `should_run`, `idle_threshold_from_secs`, `idle_gap_since`. Fourteen unit tests.
  `resume_compaction_idle_secs` is a new headless setting, defaulting to
  `RESUME_IDLE_THRESHOLD_SECS` so the setting's default and the gate's cannot drift. The caller is
  `spawn_resume_compaction` in `routes.rs`, fired from `GET /api/v1/sessions/:id/messages`.

  **The reopen is the resume signal, and it had to be a user action rather than a clock.**
  Consolidation's gate guards "never merely because the process has been up a while". The same
  hazard wears a different costume here: at boot *every* stored session has a gap of days, so a rule
  that asked only about `idle_gap` would compact the entire history store on startup and call each
  one a resume. `ResumeGateInputs::reopened` is that guard, and nothing in `pond-core` can set it
  from a timer — the one production caller sets it from a request a person made. Removing the check
  makes `a_huge_gap_alone_is_not_a_resume` and `no_single_precondition_can_be_dropped` fail with
  *"the gate ran with `reopened` unsatisfied"*.

  **What actually runs on resume today is the rolling-summary refresh, not a "full compaction", and
  the difference is worth stating plainly rather than papering over.** 3.2 says compaction "happens
  *during* the first turn back, while the user waits on a token stream". With hybrid compaction on,
  that is only half true: the in-turn work is the deterministic trimmer, which is cheap and cannot
  be pre-run anyway, because it is a function of the turn being assembled. The expensive, pre-runnable
  half is the summary — and there the phase found a real hole rather than a hypothetical one. The
  idle refresh loop in `pond-server` skips every session whose `updated_at` predates process start
  (its own "never at startup" guard), so a conversation from before the last restart keeps a summary
  that stops where it stopped, and the trimmer splices that stale summary into every turn until four
  new messages accumulate. Refreshing on reopen closes exactly that, and does it while the user is
  reading history rather than waiting on tokens. The large tier's re-summarisation is P2's, and since
  2026-08-06 this gate calls it: both triggers share `run_compaction_pass`, which P2 extended.

  **Too short is the failure that costs something, so both fallbacks lengthen.**
  `RESUME_IDLE_THRESHOLD_SECS` is 30 minutes — 15x the default `summary_idle_secs` and 2x
  consolidation's `INACTIVITY_THRESHOLD_SECS`, which is the point at which the rest of the system
  already considers the household asleep. A threshold that is too *large* only means the user pays
  what they pay today; one that is too small reads a mid-conversation pause as a resume and spends a
  model call between every pair of turns. So `MIN_RESUME_IDLE_SECS = 300` floors whatever is stored
  (a `0` from a hand-edited row must not mean "every reopen"), and `idle_gap_since` returns
  `Duration::ZERO` for a future timestamp rather than an enormous positive gap — clock skew reads as
  "active", never as "stale".

  **Invariant 1 is held structurally, not by promise.** Every input the gate needs, including its two
  database reads, is gathered *inside* the spawned task, so `GET /sessions/:id/messages` returns at
  exactly the speed it did before. The refresh itself races a watcher on `last_user_activity` — the
  same contract the idle loop uses — so a user turn cancels it and nothing is persisted. A
  process-local in-flight set keeps two rapid reopens of one session from queueing two model calls
  ahead of that turn on the serial on-device engine; it is deliberately not a database row, which
  would outlive a `kill -9` and strand the session as permanently compacting.

  **What is deferred, and named so it is not mistaken for done.** Age-weighted retention is P3's and
  is untouched. `PrefixCacheState` is P5's: this phase uses the idle gap as the cache-age proxy the
  design itself offers ("the KV cache is long gone"), and introduces no cache-state type that P5
  would then have to reconcile with. There is no integration test asserting the refresh completed
  before the first token of the next turn (section 7); that needs a live server and belongs with
  `scripts/live-test.sh`.
- **P5 — CODE LANDED 2026-08-06. MEASURED ON THE ORIN 2026-08-11, and the first clause is now
  settled with a number behind it; the second clause is still open. See
  [`docs/developer/orin-prefill-measurement.md`](../../developer/orin-prefill-measurement.md).**

  **What the measurement says.** On the Orin Nano 8GB with the headline `gemma-4-E2B-it-Q4_K_M`,
  prefill runs at 674 tok/s at 512, peaks at 976 at 4 096, and falls to 820 at 16 384 — so a cold
  prefix costs 4.19 s at 4k and **19.97 s at the pond's real 16 384 window**, growing faster than
  linearly because the rate itself degrades with depth. Decode is flat at 30.35 tok/s.

  **Caveat that qualifies every absolute number above: the sweep used the STANDALONE llama.cpp
  (`0ef6f06`, installed by `scripts/jetson/llama-optimization/`), not the tree `llama-cpp-sys-2
  0.1.146` vendors and `goose-local-inference` actually links.** Decode is memory-bandwidth-bound
  and transfers between builds; prefill is compute-bound and does not necessarily. The SHAPE — two
  orders of magnitude between prefill and decode per token, degrading with depth, seconds for a
  full re-prefill against a fraction of one for a warm turn — is what the argument rests on, and it
  survives a third either way. Quote the numbers as "this hardware and model on a neighbouring
  build", not as engine facts.

  **First clause — "cold-cache recompaction shows no TTFT penalty" — HOLDS.** When the prefix is
  already cold the turn pays that prefill whatever the trimmer decides, so recompacting at that
  moment is free: the work is already owed. The disjunction is the right shape.

  **And the warm half turns out to matter far more than the rule's phrasing suggests.** The cost of
  a needless invalidation is not a percentage, it is roughly 0.1 s versus 20 s on the same turn — two
  hundred times. That is the argument for the age rung being conditional rather than periodic.

  **Second clause — "warm-cache turns show no new re-prefills" — STILL OPEN, and honestly so.**
  `llama-bench` starts cold every run and `llama-cli`'s `--prompt-cache` is gone from the build on
  the box, so the warm figure above is arithmetic (delta ÷ rate), not an observation. Demonstrating
  that GIAP's engine actually retains a prefix turn over turn needs a TTFT trace from a deployed
  GIAP binary; `nano` is at `445fd391`, well behind this branch. **Do not promote this bullet to a
  bare LANDED on the strength of the first clause alone.**

  The original text of this stamp follows, because the code claims in it are unchanged: Section 7 asks for TTFT on the turn following a compaction, warm-cache versus cold-cache,
  on the Orin and the Mac, and says plainly that "P5 is only correct if cold-cache recompaction
  shows no TTFT penalty and warm-cache turns show no new re-prefills". No such run exists. This is
  the second measurement-pending P5 on the ledger — PAI-3's is the other — and it is recorded as
  such rather than as LANDED, because a green unit suite proves the rule fires where it was told
  to, not that firing there was cheap.

  `models/services/context/prefix_cache.rs`: `InvalidationReason` (the six reasons, exactly as
  designed), `PrefixCacheState { built_at, hash, turns_served, invalidated_by }` with
  `new`/`invalidate`/`rebuilt`/`serve_turn`/`age_since`/`posture`/`posture_of`, and `CachePosture
  { Warm | Cold }`. Nine unit tests, pure — no clock read anywhere in the file, the shape P4's
  resume gate proved. `models/ports/agent.rs` gained `Agent::prefix_cache_state() -> Option<
  PrefixCacheState>`, defaulting to `None`. `turn_trimmer.rs` takes the posture as an eighth
  argument. `goose_agent.rs` carries the state and records all six reasons.

  **The rule turned out to be one half of a disjunction, because P3 had already built the other
  half without being able to name it.** 3.3 reads as two rules — recompact when cold, prefer
  byte-identical edits when warm — and the warm one was landed on 2026-08-06 by P3, whose
  age-weighting rung fires only once the conversation is over budget and cites invariant 4 for it.
  What P3 could not do was tell "the prefix is warm" from "the prefix is already gone", so it used
  *over budget* as a proxy for both. P5 supplies the real signal and spends it in exactly one
  direction: the rung now also fires when the cache is provably cold, on a conversation that still
  fits. Nothing on the warm path was tightened. Stripping stale `<system-context>` and refreshing
  the rolling summary are front edits too and still run every turn — gating those needs the number
  section 7 is waiting for, and a threshold invented without it would be indefensible.

  **The six reasons are recorded where they happen and not one line later, and the ordering is the
  whole difficulty.** `PromptChanged` at the static-prefix comparison (plus unconditionally on the
  legacy `prefix_cache_prompt = false` path, which rebuilds the prompt every turn);
  `ModelSwapped`/`ProviderRebuilt` split by `GooseAdapter::provider_change_reason` at the completed
  swap; `ProviderRebuilt` again at the thinking-flag re-stamp, whose own comment already said "the
  KV prefix is being rebuilt this turn regardless"; `SessionResumed` on entry to
  `hydrate_goose_session`; `ToolSetChanged` in `track_user_extension`/`untrack_user_extension`;
  `MultimodalTurn` where `turn_images` is taken. The last of those sits deliberately between the
  prefix comparison and the trim, so the trimmer reads *this* turn's posture rather than the
  previous turn's.

  **The ordering bug I wrote first, and the state machine that exists because of it.** The obvious
  `serve_turn` clears `invalidated_by` — a turn was served, so the cache held. It is wrong, and
  wrong precisely on the case 3.2 calls the most valuable cold moment there is. `SessionResumed` is
  recorded while the engine session is being hydrated, hundreds of lines *before* the static prefix
  is compared in the same turn; an eager clear had a resumed session clearing its own resume and
  reading `Warm`. So the clear is deferred by one turn: `serve_turn` only clears when a turn had
  already been served on this prefix. `rebuilt` clears the reason instead, which is safe because a
  new prefix has `turns_served == 0` and `posture()` reads that as `Cold` on its own. The residue
  is that a reason is sticky for one extra turn — most visibly after a multimodal turn — so the
  state over-reports `Cold` once. That is a bounded over-count of a condition whose only
  consequence is permission to degrade material already past the verbatim horizon; the opposite
  error silently forfeits the free recompaction the phase exists to take.

  **A latent defect in P3's rung, which only became routine under P5, and it is a good argument for
  running the idempotence property on every new trigger.** `truncate_head_tail` is not a fixed
  point of itself: it keeps `cap` characters and then appends an elision marker, so its output is
  always longer than the cap and handed its own output it cuts again. P3 never saw it, because its
  rung only ran when the conversation was over budget and one cut usually brought it under. P5 runs
  the same rung on cold turns that are comfortably within budget, so without a guard a long cold
  session would have ground one tool result away by degrees, turn after turn — and section 7's
  "compaction is idempotent on an unchanged conversation" is exactly the property that forbids it.
  `AGED_FIXED_POINT_CHARS` (the aged cap plus a 64-character marker allowance) is the guard, and
  `the_aged_cap_is_a_fixed_point_after_one_cut` fails if the marker ever outgrows the allowance
  rather than letting the property rot silently. Being generous costs at most 64 uncut characters
  on an already-degraded result — which is the "marginal token saving" invariant 4 says not to move
  a prefix for.

  **`turns_served` carries no threshold, deliberately.** Invariant 4 protects "a warm prefix that
  has served many turns", which invites a rule of the form "served fewer than N turns, perturb it
  freely". That is the widening direction and it has no more measurement behind it than the warm-path
  gating above. The only use made of `turns_served` is its zero case, which needs no threshold and
  cannot be wrong: a prefix that has served nothing has nothing to protect. It is also what makes
  `rebuilt` safe to write the way it is.

  **What the port method buys, and the honest limit of "plumbed".** `prefix_cache_state()` is read
  through the trait at the live trim site rather than off the adapter's own field, so a future
  reader — P7's endpoint, which owes the user "whether the prefix survived" — sees exactly what the
  trimmer acted on rather than a second reading of the same lock. Its `None` default resolves
  through `posture_of` to `Warm`, in one place: an agent that cannot see a cache gets pre-P5
  behaviour, and `an_agent_with_no_prefix_cache_resolves_to_the_warm_posture` pins that direction,
  because `Cold` is the permission and defaulting to it would widen the rule to every mock and every
  HTTP path.

  **`CompactionProfile` gained no field**, holding the property P1 and P4 both protected: the
  trimmer's test module builds it with exhaustive literals, so a new field is an `E0063` there.
  The posture travels as an argument instead. The thirty existing call sites in that test module
  were not touched either — a shadowing warm-cache shim named `trim_history` sits at the top of the
  module, so the tests that predate P5 keep asserting warm behaviour and the P5 tests call
  `super::trim_history` directly, which is also how a reader tells the two apart.

  **Not done, and named.** No live-server run and, more importantly, no device run: the two
  measurements section 7 asks for are the phase's own acceptance criteria and neither has been
  taken. Until they are, the honest claim is that the rule is expressed, reachable in production,
  and cheap to reverse — not that it pays.
- **P6 — LANDED 2026-08-06. The predicate now moves the server, and the phrase "and after
  compaction" turned out to be two different calls.** `context_monitor.rs` gained
  `ContextMonitor::claim_compaction`, `ContextMonitor::note_compacted`,
  `ContextState::turns_at_last_compaction` and `COMPACTION_COOLDOWN_TURNS = 3`; the health arithmetic
  moved to a free `health_of(&ContextState)` so the predicate that *reports* pressure and the one
  that *acts* on it cannot drift. Eight new unit tests (`pond-core` 755 lib, up from 747).
  `routes.rs` gained `spawn_pressure_compaction`, called from the `should_compact` branch of
  `chat_stream`, and `run_compaction_pass`, the shared body P4's `spawn_resume_compaction` now also
  uses. `reset_session` gained its first production caller, in `delete_session`, with a wiring test
  (`crates/pond-api/tests/context_monitor_reset_test.rs`).

  **What "between turns" cost, and why it is the whole phase.** The tempting implementation is to do
  the work where `should_compact` is read. That line is inside the SSE generator: the model has
  finished, but the `done` frame is unsent and the client is still on the stream, so a summarisation
  there sits between the user's last token and the end of their turn — on the tier that can least
  afford it, and in direct violation of invariant 1. `spawn_pressure_compaction` therefore does
  nothing but `tokio::spawn`; the settings read, the gate, the provider read and the model call all
  happen in the detached task, exactly as P4 does it. The `context_warning` frame is untouched: it
  fires on the same predicate, carries the same fields, and reaches the client before the spawn.

  **The rate limiter is the part the design bullet does not contain, and without it the phase is a
  regression.** `should_compact` was cheap to be wrong about while it only decided whether to emit a
  frame. The moment it drives a model call it is a *rate*, and utilisation is monotone: a session
  that crosses 75% is above 75% on every later turn, so an unlimited rule summarises between every
  pair of turns. `claim_compaction` grants at most one pass per `COMPACTION_COOLDOWN_TURNS` recorded
  turns, recomputes health under its own lock so two turns finishing at once cannot both be
  authorised by one snapshot, and stamps the cooldown on the **claim** rather than on completion —
  the model call is spent whether or not the summariser finds anything to do.

  **`reset_session` after compaction is wrong, and the phase says so rather than shipping it.** The
  bullet reads "on clear and after compaction" as though one call served both. It does not. A clear
  means the session stopped existing, and dropping the whole entry is right — that is
  `delete_session`, which is also the only route where a session stops existing (`DELETE
  /sessions/:id/user` releases an identity binding and changes nothing about the context, so it is
  deliberately *not* a call site). After a compaction the session is still here and its window did
  not shrink: the rolling-summary refresh changes what the trimmer splices, not what the engine
  reports on the next turn. A full reset there would zero utilisation, empty the growth window and
  clear the cooldown stamp, and the pass would fire again on the very next turn — the failure the
  cooldown exists to prevent, reintroduced by the line meant to tidy up after it. `note_compacted`
  drops only the growth samples, which genuinely measured a differently-shaped history, and it runs
  only for `RefreshOutcome::Refreshed`: `NothingToDo` and `Cancelled` changed nothing, and paying a
  growth window for them would be pure loss.

  **What actually runs is the rolling-summary refresh, the same mechanism as P4**, because it is the
  only compaction mechanism that exists to call — the trimmer is a function of the turn being
  assembled and cannot be pre-run. P2 landed the large tier's re-summarisation on 2026-08-06, in
  `run_compaction_pass`, so on that tier a pass is now a refresh followed by a rebuild. The two triggers
  are genuinely independent (P4 is the time axis, P6 the pressure axis) and now share one in-flight
  set, renamed `COMPACTIONS_IN_FLIGHT`: a session reopened after a long gap is precisely the session
  most likely to saturate on its first turn back, and two separate sets would have stacked two model
  calls in front of one turn. No `ModelClass` gate was added, deliberately —
  `permits_compaction_model_call()` gates the large tier's re-summarisation, and applying it here
  would switch the rolling summary off on the small tier, which P1 argued against at length.

  **Not done, and named.** No live-server run. This adds a route side effect and a background model
  call, which is what `scripts/live-test.sh` exists to catch; the integration assertion section 7
  asks for — that a pass completes before the next turn's first token — still needs a live server.
- **P7 — SPLIT 2026-08-06. P7a (the API half) LANDED; P7b (the desktop control) RESPECIFIED and
  outstanding.** The bullet as written was `POST /sessions/{id}/compact` plus a desktop control on
  the existing `ContextCard`. The second half of that sentence is wrong about the app, and finding
  out how wrong is what split the phase.

  **What shipped.** `routes.rs` gained `compact_session`, registered at
  `POST /api/v1/sessions/{session_id}/compact` in the protected router, plus two small helpers:
  `compaction_in_flight` (a read-only peek at `COMPACTIONS_IN_FLIGHT`) and `compaction_report` (the
  response body). Five integration tests in `crates/pond-api/tests/manual_compaction_test.rs`.
  `MockSettingsRepository` gained round-trip support for `hybrid_compaction_enabled` and
  `context_monitor_enabled`, which it had been silently dropping — both default to `true`, so any
  test of "does turning compaction off turn it off" would have passed while the feature ran anyway,
  and the first draft of the switch test did exactly that.

  **The endpoint is a third trigger, not a third mechanism.** P4 is the time axis, P6 the pressure
  axis, and this is a person deciding; all three run the same `run_compaction_pass`. Everything that
  makes a pass safe already lives in that function — the in-flight claim, the cancellation watcher,
  the large tier's gated rebuild — so the manual axis inherits it rather than reimplementing it.

  **It does not bypass P6's rate limiter, and that is the phase.** The tempting shape is "the user
  asked, so just do it". `claim_compaction` exists because `should_compact` is monotone: past 75% a
  session is above 75% on every later turn, and an ungated rule spends a summarisation model call
  between every pair of turns on the device least able to afford one. A button that skipped the claim
  would hand exactly that ability to anything holding a bearer token, and on the serial on-device
  engine every one of those calls is time the user's next turn waits behind. So the endpoint is
  strictly a *subset* of what the pressure axis already does on its own: it can bring a pass forward
  within the rules, never past them. This programme's rule is that a scope-widening default is a bug,
  and a bypass here is the widening.

  **What that costs, said plainly, because it is a real cost.** Two presses a user would expect to
  work do not. A session below the threshold answers `not_under_pressure`. A session the monitor has
  never recorded a turn for — anything from before this process started, and everything at all when
  `context_monitor_enabled` is off — is in that same state, because utilisation is only ever learned
  from a turn. Making the button work under no pressure needs a rate limit that does not exist yet:
  the cooldown is counted in *recorded turns*, and with no turns happening it never expires, so a
  manual axis with its own gate needs a wall-clock one. That is a phase, not a line, and it is not
  this one.

  **This one awaits where the other two spawn.** P4 and P6 `tokio::spawn` and return because they run
  inside a request the user did not make — a reopen, and an SSE generator with the `done` frame still
  unsent — and invariant 1 says compaction never blocks a turn. Neither applies here: this request
  *is* the compaction, nobody is watching a token stream, and awaiting is the only way to report what
  the pass did. The turn-preemption watcher still holds, so a user who presses the button and then
  starts typing cancels their own pass and persists nothing.

  **Two orderings differ from `spawn_pressure_compaction` on purpose.** It reads the claim before the
  provider, because the claim is the cheap check and the one that fails most often. This reads the
  provider *first*, and peeks at the in-flight set before claiming, because a claim spent on a pass
  that then cannot run burns a cooldown counted in recorded turns — and a person pressing a button is
  very often taking no turns at all, so the control would stay dead until they did. Two of the five
  tests assert exactly that: a refusal for "the feature is off" and a refusal for "not under
  pressure" both leave the next real claim available.

  **Everything short of a server fault answers 200 with a `status`/`reason` pair.** "I did not
  compact, here is why, and here is where your window actually is" is a successful answer to this
  question, and a manual control that silently no-ops is indistinguishable from a broken one. 404 is
  reserved for a session id that does not exist, so a typo is not reported as a healthy window. The
  reported context block is deliberately the same four fields the `context_warning` SSE frame carries,
  read out of the same `ContextHealth`, so the number on the button and the number the stream pushed
  cannot drift — which is also what section 7's manual check ("verify the reported numbers match a
  subsequent `turn_stats`") is asking for. One departure: `turns_remaining` is `null` rather than
  `u32::MAX`, which the frame still serialises as 4294967295 and which reads to a client as a number.

  **The guard, and what breaking it looks like.** `a_second_press_is_refused_by_the_cooldown` is the
  mutation-tested one. Its first draft asserted `status == "skipped"` and would have passed against a
  bypassed limiter: a second pass finds the through-pointer already advanced and answers
  `NothingToDo`, which is *also* reported as skipped. The assertion that actually holds is
  `outcome is null` — null exactly when no pass ran. Disabling the claim makes it fail with "the
  second press ran a second pass - the manual endpoint is bypassing P6's rate limiter…" and the whole
  body, showing `"outcome":"nothing_to_do"`.

  **P7b, respecified.** The design bullet's "a desktop control on the existing `ContextCard`" does not
  describe this app. `pond-desktop/src/components/ContextCard.tsx` is the MCP-UI tool-result card
  renderer: it takes a `ContextCard` off `state/reducer.ts` and dispatches to the weather, devices,
  memory and schedules renderers. "Context" there means tool-call context, not context window, and it
  has nothing to do with compaction, utilisation or pressure. A grep of all of `pond-desktop/src` for
  `context_warning`, `contextWarning`, `should_compact`, `context_health` and `percent_used` finds
  nothing: the `context_warning` frame the server has emitted since before PAI-4 has **zero consumers
  in the shipped app**, and `context_warning` is not even in the `ChatEventType` union in
  `api/types.ts`. So P7b is a new surface — a frame the client learns to read, a pressure indicator,
  and a control that calls this endpoint and renders its `reason` — across both chat surfaces
  (`Chat.tsx` and `Canvas.tsx`), not a button added to an existing card. It is scoped and estimated as
  such, and it is deliberately not bundled into the API landing: AGENTS.md's "know what has no UI
  before writing a UI test for it" applies exactly here, and a Playwright test written against the
  assumed `ContextCard` control would have exercised nothing and passed vacuously.

  **Not done, and named.** No live-server run: this adds a route, and `scripts/live-test.sh` is what
  catches route registration and auth-middleware problems that no in-process `oneshot` router test
  can see. Section 7's manual check is still manual. And the endpoint has never been exercised against
  a real on-device summariser, only `MockProvider`.
- **P7b — PARTIALLY LANDED 2026-08-06.** The frame now has a consumer in the hub chat. The classic
  chat section is blocked, not skipped, and the reason is recorded below.

  **The respec above named the wrong pair of surfaces, in both directions.** There are three
  `api.chatStream` callers, not two: `sections/Chat.tsx`, `sections/Canvas.tsx` and
  `hub/views/ChatHub.tsx`. The one the paragraph omits is the hub chat — the newer of the two UIs
  Jerry keeps deliberately blended — and it is also the one that already implements the
  `turn_limit_reached` per-turn affordance this phase should copy. `Canvas.tsx` is the surface that
  should arguably have been excluded: its thread items carry no id, and it implements neither
  `turn_stats` nor `turn_limit_reached`, so a per-turn control there is a new pattern rather than
  parity. So Canvas is a **deliberate deferral**, written down here so the next reader does not log it
  as an oversight.

  **What shipped.** `context_warning` is in the `ChatEventType` union; `ContextWarning` and
  `CompactionReport` are exported from `api/types.ts`; `PondApiClient.compactSession` posts to
  `/api/v1/sessions/{id}/compact`; and a new shared `components/ContextPressureNote.tsx` renders the
  pressure line plus a "Compact now" control, styled in `hub/views/chat.css` next to `.turn-limit`,
  the file both full chat surfaces already share. `ChatHub.tsx` gained the `context_warning` branch
  (attaching by message id, same reasoning as `turn_stats`) and renders the note beside the turn-limit
  one.

  **No Rust changed, and that is the finding.** The paragraph above proves zero consumers by grep,
  which is true but is a *rendering* fact rather than a transport one: `PondApiClient.streamSse`
  yields every parsed frame unfiltered and every consumer chain drops unknown types silently, so the
  frame already reached the client and nothing needed plumbing. It is produced under
  `context_monitor_enabled`, which defaults to `true`.

  **Two wire details the respec flattened.** It calls the endpoint body and the SSE frame "the same
  four fields". They differ in exactly the two places a client gets wrong. `turns_remaining` is `null`
  on the endpoint and a raw `u32` on the frame, where `u32::MAX` (4294967295) means "growth unknown" —
  the component clamps it and a test asserts the digits never render. And `warning` is only written
  above 60% utilisation while `should_compact` also fires through the `estimated_turns_remaining < 3`
  limb, so `warning: null` on a `context_warning` frame is a producible state, not a defensive `?`;
  the component composes a utilisation line for it and a test asserts that too.

  **`TurnStatsFooter` was considered and rejected as the host.** That footer is gated on
  `show_turn_stats`, which defaults to `false` in Rust — putting the control there would have shipped
  it invisible on every default install, which is the same class of mistake as PAI-4 P2 being correct
  and unreachable.

  **The guard, and the mutation.** `ContextPressureNote.test.tsx` holds the behaviour tests and, more
  importantly, the wiring assertion that would go red if this phase were reverted: it reads
  `ChatHub.tsx` from disk and fails with "the frame has no consumer in ChatHub.tsx" when the branch is
  deleted. That assertion is the whole point of the phase — the component can be perfect and the
  feature still worth nothing if nothing renders it. Four mutations were run: removing the
  `warning: null` fallback, removing the `u32::MAX` clamp, turning a refusal into an error
  (`role="alert"` plus the word Error), and deleting the `ChatHub` branch. Each failed naming the real
  problem; all were restored and the full suite is 261 passing.

  > **CORRECTED 2026-08-06 (synthesis): that assertion is a two-substring grep, and calling it "the
  > assertion that would go red if this phase were reverted" is too strong — it goes red only for a
  > revert done with a delete key.** The test asserts `src.includes('ev.type === "context_warning"')`
  > and `src.includes("<ContextPressureNote")` against `ChatHub.tsx` read off disk. Two semantic
  > mutations that leave both strings in place passed 261/261: (a) adding `showTurnStats` to the
  > render guard — `show_turn_stats` defaults to FALSE in Rust, so the note ships invisible on every
  > default install, which is *the exact failure mode this stamp says it deliberately avoided by
  > rejecting `TurnStatsFooter`*; and (b) attaching the frame to `m.id === "no-such-message"`, so it
  > lands on nothing and can never render. Nothing in the suite ever renders `ChatHubView`, so no
  > test observes whether the note appears.
  >
  > Keep the grep — it is a cheap tripwire and it does catch a deletion — but it is not coverage.
  > What is needed is one render test that mounts `ChatHubView` (or extracts the reducer mapping a
  > `ChatEvent` onto a `ChatMessage`) and asserts a `context_warning` followed by `done` produces a
  > `.ctx-pressure` node. That test dies under both mutations above; the grep dies under neither.
  > Not written here: it needs the `AppContext`/api surface stood up, which is a frontend phase, and
  > it should land together with the `sections/Chat.tsx` half this stamp already lists as blocked.

  > **OPEN 2026-08-06 (synthesis), and it is the more serious of the two: the "Compact now" control
  > can never succeed on any default configuration.** — **CLOSED 2026-08-06 by P7b-fix, below; the
  > diagnosis was exact and the reproduction is now a test.** P6's pressure axis and P7b's manual axis share
  > both the trigger condition and the single `claim_compaction` quota, and the pressure axis takes
  > the claim strictly BEFORE the button is rendered. In the chat-stream generator the
  > `context_warning` frame is yielded and `spawn_pressure_compaction(&state, &session_id)` is called
  > **one statement later**, inside the same `if health.should_compact` block. `claim_compaction`
  > then stamps `turns_at_last_compaction` for `COMPACTION_COOLDOWN_TURNS` = 3 *recorded turns*, and
  > `compact_session` answers `cooling_down` when the claim fails. The note only renders once
  > `m.streaming` is false, i.e. strictly after the `done` frame, which is yielded after the spawn —
  > so this is not a race a user can lose, the claim is already gone before the button exists.
  > Measured by a reviewer's temporary probe reproducing the production ordering: six consecutive
  > pressured turns, six identical `status="skipped" reason="cooling_down"` answers, never one
  > `compacted`.
  >
  > So the advertised success path — `status: "compacted"` and the "Compacted — the window has room
  > again." string — is unreachable, not merely uncommon. This is the same class as the recorded
  > "PAI-4 P2 is correct but untriggered", except that here the phase's reachability claim asserts
  > the opposite, which is why it is corrected rather than filed.
  >
  > **Deliberately not fixed by the synthesis pass, because it is a design decision and not a
  > repair.** Two options, and they differ in what they believe the cooldown is FOR. (a) Give the
  > manual axis its own authorisation: keep `COMPACTIONS_IN_FLIGHT`, which is what actually protects
  > the serial on-device engine, and let a human press bypass the *turn* cooldown, on the argument
  > that the cooldown exists to ration AUTOMATIC model calls. (b) Stop rendering the control when
  > the pressure axis has already claimed — the server would have to say so, e.g. an
  > `auto_pass_started` field on the `context_warning` frame — and the note becomes information
  > only. Either way P7a's guard `a_second_press_is_refused_by_the_cooldown` must be re-derived: it
  > currently asserts the cooldown, which is exactly the thing that makes the button dead. Note that
  > P7a's own stamp already argued hard for *not* bypassing the limiter; whoever takes this should
  > read that argument first and then decide, rather than treating this as an obvious bug fix.

  **Blocked, not deferred: `sections/Chat.tsx`.** That file was held by the concurrently-running PAI-5
  reasoning group in this batch, so the identical branch and render are NOT in the classic chat
  section, and neither is `Chat.test.tsx` nor the `context_warning` case in `tests/e2e/chat.spec.ts`
  (that spec drives the Chat section, so it would have exercised nothing). Applying the same six-line
  branch at the `turn_limit_reached` site and the same render beside it finishes the phase. The e2e
  helper already carries a default `**/api/v1/sessions/*/compact` mock returning a
  `not_under_pressure` refusal, so no spec can reach a real network when it does.

  **What would falsify this.** A live session that crosses 75% and shows no note in the hub chat —
  which is also the state `POST /sessions/{id}/compact` answers `not_under_pressure` for, so the
  honest check is a long session, not a fabricated frame. Note that utilisation is only ever learned
  from a turn taken since this process started, so most real sessions never reach the state at all;
  the refusal renderings are what a user actually hits, and they are what the tests assert hardest.
- **P7b-fix — LANDED 2026-08-06.** Both open items above are closed: the "Compact now" control can
  now succeed, and the classic chat section renders it. Neither half is a line-count change, because
  the first is a decision about what the cooldown is *for* and the second is the render test round
  one could not write.

  **The button, and why the fix is not a bypass.** `ContextMonitor::claim_manual_compaction` sits
  beside `claim_compaction` and differs from it in exactly one respect: it skips the
  `COMPACTION_COOLDOWN_TURNS` check. It still requires the session to exist, it still recomputes
  `should_compact` under the same lock — a person may not compact a session that is not under
  pressure, and that limb was never what made the button dead — and it **still stamps**
  `turns_at_last_compaction`. Consuming the quota without checking it is the whole design: a human
  press rations the automatic axis for three recorded turns afterwards exactly as an automatic pass
  would, so the two axes cannot between them buy two summarisations for one turn. The manual axis
  gains nothing the pressure axis did not already have; it only stops being refused by a claim the
  pressure axis took on its behalf one statement earlier.

  **P7a's argument was read before deciding, as its stamp asked, and it is a threat model rather
  than a mechanism.** The mechanism in the code cuts the other way. `SessionSummaryService::refresh`
  decides `NothingToDo` from the rolling summary's through-pointer and the message count *before* it
  reaches `provider.complete`, so a client hammering the endpoint after one successful pass spends a
  database read per press and no model call — and every message that would change that answer is a
  turn a human had to take. What bounds the manual axis's model spend is the through-pointer, not the
  turn cooldown, which is precisely why removing the cooldown from this axis costs nothing P7a was
  protecting. `compaction_in_flight` is untouched and is read *before* the claim; that is the guard
  that actually protects a serial on-device engine, and it survives.

  **One residual, named here rather than discovered later.** `SessionSummaryService::resummarise` is
  *not* through-pointer-idempotent in the same way and can spend one model call per press. It is
  gated on `ModelClass::permits_compaction_model_call()` — the large tier only, definitionally not
  the on-device engine the argument above is about — and it shares this pass's single
  `COMPACTIONS_IN_FLIGHT` claim, so it serialises. If a wall-clock rate limit is ever built, this is
  the path that wants it.

  **`cooling_down` is now unreachable from this endpoint, and the refusal was re-labelled rather
  than left lying.** With the cooldown skipped, the only way the claim can refuse after the
  `should_compact` read a few lines above is that a turn completed in between and took the session
  back under the threshold. That is `not_under_pressure`, and reporting it as `cooling_down` would
  have been a sentence about a rule this axis no longer runs. `ContextPressureNote`'s `REASON_TEXT`
  keeps its `cooling_down` entry — the component was outside this phase's footprint, and a spare
  entry is harmless where a missing one would print "Nothing was compacted." for a real refusal.

  **The guard was RE-DERIVED, not weakened, and that is the point of the phase.**
  `a_second_press_is_refused_by_the_cooldown` asserted the very behaviour that made the button dead
  and passed while doing so — a test can be green and be certifying the bug. It is replaced by four:
  `a_press_after_the_pressure_axis_already_claimed_still_compacts` (the guard: it calls
  `claim_compaction` FIRST, reproducing what `spawn_pressure_compaction` does one statement after the
  frame, and then presses); `a_press_rations_the_automatic_axis_afterwards` (the non-widening
  control); `a_press_while_a_pass_is_in_flight_is_refused` (the surviving guard, made deterministic
  by a provider that blocks inside `complete` until the test releases it, rather than by racing a
  100 ms sleep); and `a_second_press_spends_no_second_model_call`, which **counts provider calls**
  because P7a already learned that a status-only assertion passes against both the bug and the fix —
  `NothingToDo` and a second real pass are both reported as `skipped`.

  Three mutations, each restored byte-identical. Putting `claim_compaction` back in `compact_session`
  failed the first test naming the pressure axis having taken the shared quota. Deleting the
  `compaction_in_flight` check failed the third with `"failed"` where `already_running` belonged.
  Dropping the `turns_at_last_compaction` stamp from `claim_manual_compaction` failed the second in
  both `pond-core` and `pond-api`, saying a press now costs the automatic axis nothing.

  **The Chat section, and the render test the synthesis correction asked for.** `sections/Chat.tsx`
  gets the same `context_warning` branch (attaching by `m.id === agentMsg.id`) and the same
  `<ContextPressureNote>` beside the `.turn-limit` block, gated on `!msg.streaming` and **not** on
  `showTurnStats` — `show_turn_stats` is false in Rust by default and borrowing that gate would ship
  the note invisible, which is why `TurnStatsFooter` was rejected as its host in the first place.
  `Chat.test.tsx` now mounts the real component, drives a real `context_warning` frame through the
  real stream and asserts a real `.ctx-pressure` node. Both mutations that beat round one's grep were
  run against it and both went red: adding `showTurnStats &&` to the render guard, and attaching the
  frame to a message id that does not exist. A vacuity control (`renders no note for a turn that
  never reported pressure`) sits beside them, and a third test asserts the button is *enabled*, since
  the frame carries no session id and a permanently-disabled control is a dead button by another
  route. The two-substring grep in `ContextPressureNote.test.tsx` is kept and now covers both
  surfaces, re-labelled in the file as a tripwire rather than as coverage — `ChatHub.tsx` still has
  no mount harness, so for that surface a grep is genuinely all there is.

  **Deliberately NOT done.** `Canvas.tsx` remains the deferral the P7b stamp already argued: its
  thread items carry no id and it implements neither `turn_stats` nor `turn_limit_reached`, so a
  per-turn control there is a new pattern rather than parity. No wall-clock rate limit, so a session
  the monitor has never recorded a turn for still answers `not_under_pressure` — that is unchanged
  and still a phase, not a line. And an agent bubble with no text, no cards and no reasoning is
  suppressed entirely, note and all; the generator that emits `context_warning` is the one that emits
  the answer, so a warning-only turn is not a state production produces, and widening the suppression
  rule to keep an empty bubble alive for a footer would be the wrong trade.

  **What would falsify this.** A pressured session where the note renders and the press still answers
  `cooling_down` — that string should now be unreachable from this endpoint. A press that compacts
  and then leaves the pressure axis free to compact again on the very next turn: the two axes are
  supposed to share one quota, and if they stop the device pays for it. A press that reaches the
  summariser twice with no new messages between. Or `.ctx-pressure` appearing in the classic chat on
  a turn that sent no `context_warning`.

---

## 5. Invariants

1. Compaction never blocks a turn. Ever.
2. Tool results never orphan from their turn.
3. On the small on-device tier, no compaction path **waits on** a model. Restated 2026-08-06 by P1:
   the original wording ("calls a model") would have switched off the idle rolling summary there, and
   that summary is never awaited by a turn — see the correction under 3.1. What this forbids on the
   two on-device tiers is the large tier's re-summarisation, which compaction does wait for;
   `ModelClass::permits_compaction_model_call()` is the gate, and it is false for `Small` and
   `Medium`. **Wired 2026-08-06 by P2**, in two layers: `should_resummarise` checks it first, and
   `SessionSummaryService::resummarise` checks it again before making even a database read. Removing
   only one of the two leaves the invariant held — which is deliberate, and which the P2 mutation
   test confirms in both directions.
4. A warm prefix that has served many turns is not perturbed for a marginal token saving.
5. The rolling summary is read, never awaited.
6. Image policy lives in `image_history.rs` and nowhere else.

---

## 6. Deliberate deferrals

- **Compacting `session_messages` itself.** The durable store is pruned by count (500/session,
  `pruning.rs`) and is the authoritative history the UI reads. Summarising it would destroy the
  record to save disk that is not scarce. Compaction stays a property of what the *model* sees.
- **Semantic compaction** (keeping turns by relevance to the current topic rather than recency).
  Interesting, needs an embedding call per turn on the critical path, and interacts badly with
  prefix stability. Revisit after P5 gives us cache-cost measurements.
- **Cross-session compaction.** Belongs to memory consolidation, which already exists.

---

## 7. Verification

- **Unit** — strategy dispatch per model class; the age-weighting priority order; `PrefixCacheState`
  transitions for all six invalidation reasons.
- **Property** — after any compaction, every retained tool result still has its request; the
  estimate never exceeds the profile budget; compaction is idempotent on an unchanged conversation.
- **Measured, Orin and Mac** — TTFT on the turn following a compaction, warm-cache versus cold-cache.
  P5 is only correct if cold-cache recompaction shows no TTFT penalty and warm-cache turns show no
  new re-prefills.
  **Mac half, 2026-09-24:** warm-cache re-prefills were present and came from goose prepending
  `<turn-context>` to a cached user message, not from compaction; fork patch `36413f065` appends it
  (471 -> 97 and 589 -> 147 tokens redone per inference on E2B). Orin half still owed. Full numbers in
  `docs/developer/realtime-inference-audit-2026-09-24.md`.
- **Integration** — resume a session after `resume_compaction_idle_secs`, assert compaction completed
  *before* the first token of the next turn.
- **Regression** — a long Jetson conversation produces no `ContextLengthExceeded` and no reactive
  Goose compaction.
- **Manual** — `POST /sessions/{id}/compact` on a saturated session; verify the reported numbers
  match a subsequent `turn_stats`.
