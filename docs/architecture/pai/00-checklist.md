# PAI working checklist — read this first, every time

This is the scratchpad and standing checklist for the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).

**If you are about to touch anything in PAI-1 through PAI-8, read this file before you start and
update it before you stop.** It is deliberately committed rather than left in `.ai/` (which is
gitignored) so it survives a fresh clone and a recycled container.

---

## 1. The eight requirements, as originally stated

Recorded verbatim in intent, because paraphrase drifts. These are the source of truth for what the
programme is for; the PAI numbers are only the order I chose to build them in.

| Req | As asked for | Doc | Status |
|---|---|---|---|
| 1 | GIAP needs to be **proactive**, not just reactive | [PAI-7](./07-proactive-intelligence.md) | **NOT VERIFIED -- RUN TWICE ON THE ORIN 2026-08-12 AND IT FAILED; the CAUSE is now fixed and the RE-RUN has not happened.** Every mechanism worked and the yield was zero: the loop fired, resolved its audience, spawned a child that answered in 32 s, and every impulse was refused because a 2B model wrote a `type` key that `ReviewerImpulse` did not have. The attribute doing the refusing was `deny_unknown_fields`, copied from `TaskRequest` -- and the safety property never depended on it, because NOTHING in that struct is a capability: audience, expiry, profile and task kind are all decided by `build_proposal` from the caller's values, and `impulse_action` returns one variant. It bought strictness against a field that could do nothing and charged every suggestion the pond would ever make. Removed 2026-08-12; `naming_the_audience_does_not_let_a_model_choose_one` now pins the stronger claim (the key is ignored AND the audience, expiry and kind still come from the caller), and `a_misspelt_required_field_is_still_refused_without_the_strict_attribute` pins the typo case that requiredness -- not strictness -- was always covering. **Re-run on the Orin 2026-08-12 (third run): the reviewer now RUNS and still yields nothing, for a NEW reason.** The `deny_unknown_fields` cause is confirmed cleared -- the review reached impulse interpretation, which it had never done before -- and the refusal is now `Unreadable { index: 0, message: "missing field `trigger_kind`" }`. A 2B model omits a REQUIRED field. That is the schema behaving correctly, and the guard `a_misspelt_required_field_is_still_refused_without_the_strict_attribute` pins it as such; the model, not the contract, is what did not hold. **The fix was necessary and not sufficient.** Relaxing `trigger_kind` is NOT the next move: it is the bus-event family a proposal is about, it feeds `ProposalShape` and therefore the suppression ledger, so a defaulted one produces proposals bound to the wrong trigger and poisons the record of what the member already declined -- strictly worse than producing none. The open design option is to have the brief's numbered event list be the answer space ("which event number is this about?") instead of asking a small model to echo a taxonomy string; that is a recipe AND schema change together, and it is the model-floor decision, not a patch. Two harness defects were fixed getting here and neither was a product fault: the probe depended on PAI-1's probe having run to create an audience, and `refusal_detail` searched raw log text for `first=` that tracing writes with ANSI codes between the name and the `=`, so it reported "no refusal detail" while the log held the answer. **This row stays NOT VERIFIED until a review yields a proposal on the device**, per the vocabulary below: a fix is LANDED, and only a run makes it VERIFIED. **P1, P2, P3, P8 LANDED 2026-08-10/11.** P1 widened `BusEvent` with `Time` and `Session`, publishers only — and its sharpest defect was that **a scheduled task fabricated the user's presence**: `schedule_executors.rs` mints `sched-{task}-{ts}` sessions, the observer read session rows without distinguishing a human's conversation from a machine's, so a cron line at 3am published `Started` and the pond believed somebody was home. Fixed at the INPUT (`SessionOrigin` / `POND_AUTHORED_SESSION_PREFIXES`, applied to the arrival list AND the activity clock together, since filtering one and not the other is how half that fix lands), and it was wider than reported — the same defect was holding background consolidation off as if somebody were typing. P2 presence from PAI-1's identification chain, keyed on when somebody last SPOKE rather than when an attribution was written. P3 the `Proposal` domain in the drafts table, plus the REST surface that disposes of one. It landed in two steps on purpose: as **P3a**, domain-and-persistence-only, with a test walking every `.rs` file in the workspace to prove NOTHING constructed the repository — because `dead_code` cannot catch an unreachable `pub` item in a library crate, which is how PAI-1 P5 was recorded as landed while inert. That test was written to fail the day the surface arrived, and it did, with a message telling its reader to delete it and fix this row in the same change. P8 the scheduler repairs: durable cooldowns (they reset on restart, so a crash-looping pond re-fired every rule), a `/rules` route, and `TaskKind::ToolCall`. **P1-P8 LANDED 2026-08-11 — and the FEATURE DOES NOT WORK ON HARDWARE, confirmed twice on the Orin 2026-08-12.** Both halves belong in one sentence. Every mechanism is correct and exercised: the schedule fires at fifteen minutes, the audience resolves, the brief is built, the delegation is authorised, the child runs and answers, the JSON array parses. And the yield is ZERO, because a 2B model does not write the schema it was given -- the first run named it, `unknown field 'type'`. PAI-7 is code-complete and product-broken; a reader who takes LANDED to mean working will be wrong here. `scripts/pai-bench.sh --slow` reports FAIL, which is the honest verdict. P4's decision half had landed the day before and was inert, saying so in its own module docs; the loop in `run_proactive_reviewer` is the caller. Three things it turned up are worth carrying: **a review must be its own parent turn**, because `GooseOrchestrator::spawn` refuses any spec whose parent session has no live registry entry — which a background loop fails by construction, so every review would have been refused with "no live turn holds the authority", a message that reads exactly like a guard working. Publishing `review_authority` is what the check was asking for, since the registry entry is what carries the cancellation token that makes invariant 3 cascade at all. **The audience window must outlive the idle threshold that starts the review**: reusing `attribution_candidates` was the obvious move and it is bounded by the same fifteen minutes a review waits for, so its answer at review time is guaranteed empty — the reviewer would have addressed nobody on every pond forever, looking like a feature nobody enabled. **And the brief goes around the scope rather than through it** — it is prose handed to the model, so PAI-6's clamp cannot see it, and `brief_events` is the only place a presence event naming a *different* member is dropped. `proactive_review_enabled` is a SECOND toggle beside `ext_orchestrator_enabled`, both off: one asks whether a model may delegate inside a turn the user started, the other whether the pond may start a turn of its own, and folding them would make delegation silently imply proactivity. P5 gained its first production caller in the same change (the reviewer's `send_to_profile`), and P7's ledger gained its READ — whose failure skips the tick, because an empty ledger suppresses nothing and a plausible `unwrap_or_default()` would re-propose exactly what a member has already declined. **Owed and not got:** a completed review on real hardware, and the offline-device delivery path. `ci.yml` never runs `cargo test -p pond-server`, so `crates/pond-infra/tests/proactive_reviewer_is_wired.rs` asserts the wiring from the fast pass **2026-09-16 -- the suggestion engine, which is deliberately NOT a proposal producer, and one PAI-7 repair it did make.** The Home column that reads `GET /api/v1/proposals` had never rendered a row, and the reviewer's zero yield was only one of four reasons. Measured on Jerry's own pond, with `proactive_review_enabled` and `ext_orchestrator_enabled` both already **true**: `select count(*) from drafts where origin='proactive'` is 0. The other three reasons are independent of the model floor. (a) `proposal_caller` answered 403 to any caller it could not resolve to one member, and an unidentified session on a ONE-member pond resolves to `Household`, which `ProposalAudience` refuses. (b) `state.sessionId` is null on a cold desktop launch and never persisted, so Home did not fetch at all. (c) a 403 and a quiet house are pixel-identical, because `SuggestionQueue` swallows every error into the quiet line. **Repair taken: `proposal_caller` now falls through to `member_attribution::sole_member` when the scope is `Household`.** Invariant 4 is untouched -- the fallthrough RESOLVES the member and hands downstream an `Owner`, so nothing ever sees a broadcast; `Guest` is deliberately NOT admitted, because `identity_resolution` answers `Guest` exactly when the household has more than one member, where picking would be the row-order attribution PAI-1 P3 refused. This is the same call `881da889` made for `context_source_owner`, whose commit message is the argument: connecting a calendar "required first starting a conversation AND being on an attributed device, to establish something a one-member pond has exactly one possible answer to". The write path had already made it -- `is_draft_decision_permitted` admits `Household` under "Single-member pond: there is no other member to protect from" -- so the READ was refusing what the WRITE permitted, the exact inversion `proposal_caller`'s own docstring says it exists to prevent. `an_unidentified_session_on_a_one_member_pond_is_still_refused` was REWRITTEN around two members rather than deleted, per `881da889`'s own rule that what it was really pinning is still true. `session_id` stays REQUIRED: `is_draft_decision_permitted` rung 1 refuses a blank actor session, so making it optional would 403 every decide after letting the list through. **What the suggestion engine is, and why it is not this workstream.** A suggestion performs nothing until somebody taps it, so the tap IS the consent and there is no staged action to approve -- which is why it needs no audience, no expiry and no interruption budget, and why routing it through proposals would have made it inert on the same ponds for the same reason. It lives in `pond-core/src/user_data/services/suggestion.rs`, is a pure function over a measured snapshot, spends ZERO inference, and is served by `GET /api/v1/suggestions` with no member gate -- the same call `list_reminders` made and records. It does not write a `drafts` row and does not touch `ProposalAudience`. **It also carries the fix for (c):** the route returns a `considered` list naming every suggestor and why each silent one was silent, so an empty column is falsifiable. **This row stays NOT VERIFIED**, unchanged: the reviewer has still never yielded a proposal on the device, and none of the above changes that. What it does change is that when the reviewer finally does yield one, there is now a pond that can see it. **2026-09-16 (later) -- the producer could not have run either, and the reason is the same one, one function further up.** The row above repaired the READ (`proposal_caller`) and left the PRODUCER unrepaired: `audience_for_review` required a human session inside `AUDIENCE_WINDOW` carrying a `profile_id`, and on Jerry's pond **0 of 961 sessions carry one**. The only writer is `resolve_turn_scope`'s `DeviceRung::Member` arm, and the desktop's own device row is never attributed, so that arm is unreachable on every desktop pond -- which means the function returned `None` on every tick of every process the reviewer has ever run. Nothing said so: the bail is `tracing::trace!`, which the shipped file filter drops entirely, so the reviewer has never cost a token and never written a line. The model floor recorded above was therefore never the binding constraint on this pond -- the Orin runs got past this gate because PAI-1's probe had created an attribution, and no ordinary household ever does. **Repair taken: the same sole-member fallthrough, applied to the producer.** `audience_for_review` now takes the household roster and, when no conversation is attributed, addresses the one member when there is exactly one. Invariant 4 is untouched by construction -- the fallthrough RESOLVES a member and returns an `Owner`, so nothing downstream ever sees a broadcast; with two members it answers `None` exactly as before, because choosing would be the row-order attribution PAI-1 P3 refused. Four tests pin it, two of them the safety half (`two_members_and_no_attribution_is_still_nobody`, `an_attribution_the_pond_made_outranks_the_roster`), and `AUDIENCE_WINDOW` still binds, so a pond nobody has touched in six hours is not addressed just for having one member. The roster is re-read per tick and a failed read counts as NO members, which makes the fallthrough unavailable rather than addressing a proposal to whoever a broken query last returned. **This does NOT make the reviewer yield.** Two gates remain downstream and both are unmeasured here: `brief_events` needs reviewable bus events in the ring, and the impulse schema still meets whatever model the pond is running -- the `trigger_kind` refusal recorded above is untouched. What the repair changes is that the gate is now reachable at all, which it has never been on a household pond. **Found because a household asked why Home only ever showed placeholder suggestions**, which is the honest description of the symptom: the column falls back to the template-tier suggestion engine precisely when no proposal is waiting, and no proposal has ever been waiting. |
| 2 | Requires the ability of the model to **think** | [PAI-5](./05-reasoning-and-thinking.md) | **VERIFIED on the Orin 2026-08-12** (pai-bench PAI-5 PASS: 156-253 reasoning tokens counted and reported). **P1, P2, P3, P4, P6 LANDED** (P1, P2 2026-08-06; P4, P6 and P1's granularity fix 2026-08-07; P3 predates the programme). P1 the reasoning channel — `GooseAdapter` lifts `MessageContent::Thinking` and yields `AgentStreamEvent::Thinking`, gated once at the PRODUCER on `show_thinking && !voice` so all three consumers inherit it; **its granularity defect is FIXED** (`76653347`) — `ReasoningCoalescer` emits one frame per passage rather than one per token, which on local/gguf was rendering hundreds of one-fragment paragraphs. P2 producer + store — `reasoning_tokens` on `UsageStats`/`TurnStats`/`SessionMessage`, migration 0039 nullable with NO `DEFAULT`, counted through PAI-3's `TokenCounter` port and reported ALONGSIDE `completion_tokens`, never deducted; the seam from `UsageStats` to the persisted row is now guarded (`b2b8eb4a`, `reasoning_persistence_guard.rs`). P4 `reasoning_effort` tri-state rendered into the prompt as a word budget derived from the window (`8689ba47`) — deliberately NOT a `CompactionProfile` field, because every production site goes through `from_context_window`, which would have minted fifteen profiles carrying an effort nobody chose. P6 persist + rehydrate (`bc3aa9df`) — migration 0040 as a SIDE TABLE so no prompt-building `SELECT` can hand a model its own scratch work, gated once in `record_thinking`, opt-in and off by default. **P7 LANDED — parity half 2026-08-10, unification half 2026-08-11** — the parity half: `/agent/chat/stream` now extracts memory from its turns, having built a `ChatService`, persisted both sides and never wired an extractor, so a whole conversation held there contributed nothing. It also now scopes that service, and **that ordering was the defect this phase was one line from shipping**: the route resolves the turn's scope after the session row exists (it must — it creates that row inside the stream body) but built the service thirty lines earlier, so it kept `ChatService`'s `Household` default. Inert while extraction was off; switching extraction on without moving the resolution would have written a Guest's turn into the whole household's memory. A widening default reached by ordering is still a widening default. `stream_handler_parity.rs` guards it on ORDER and ARGUMENTS, not presence. **The unification half did NOT land** and is its own change: both handlers cover the same ten `AgentStreamEvent` variants with the same frame JSON, but `chat_stream` is a wrapper delegating to `chat_stream_inner` while `agent_chat_stream` is inline, and only one accumulates per-tool telemetry — worth doing before PAI-6 P6, which otherwise writes its arm twice. **The unification half landed 2026-08-11**: `routes.rs` now holds ONE match on `AgentStreamEvent`, inside `TurnAccumulator::absorb`, where it held two — so PAI-6 P6 wrote its new variant's arm once rather than twice into two blocks that had already drifted. The two things the routes legitimately differ about come back as DATA rather than being resolved in the translator, which is what avoided an `is_agent_route: bool`: reasoning is handed back for each handler's own `ChatService` to gate, and `Done` comes back as numbers because *when* a route closes its stream is a routing decision, not a frame shape. **P5 LANDED 2026-08-11 — PAI-5 is COMPLETE, P1-P7.** The anchor's 768 read like the phase and was not it: one observation in a constant, and the cost of reasoning is a property of the model and the effort. `observed_output_reserve` sizes it from this pond's own `reasoning_tokens` — P2's data, which had no reader until now — quantised to 256 tokens so a per-turn recomputation cannot shift the trim point and truncate the KV prefix every turn. Floored at the anchor, so observation may only raise it; `None` counts are dropped rather than folded in as zero. **P2's two display surfaces landed** — the `turn_stats` SSE frame and `GET /usage/summary` both carry the reasoning count on BOTH stream routes, guarded by `reasoning_reaches_the_edge.rs`; the clause here saying they remained undone was stale from 2026-08-06, when `routes.rs` was held by other work. **P8 LANDED 2026-08-12 — what thinking costs when it goes WRONG.** P2 and P5 measured the cheaper half: a turn that thinks, says nothing and ends is re-engaged by `EMPTY_TURN_STEER` and paid for AGAIN in full, prefill and tool schemas included, and gemma-4-E2B does this reliably for certain phrasings. Nothing counted it, so the cost of thinking could exceed the reasoning tokens themselves with no way to tell. `TurnStats::reengagements` comes from the `'attempts` counter, migration 0008 (logs DB) adds `reasoning_tokens` + `reengagements` to `turn_metrics` both nullable with no `DEFAULT`, and the SSE frame and `pai-bench` report it. It is deliberately NOT derivable from `inference_count`, which cannot tell a re-engagement from a tool round-trip. `#[serde(default)]` on the field is load-bearing: `TurnStats` rides `AgentStreamEvent::Done` across the `--json-events` NDJSON process boundary, and requiring the field broke two tests immediately — a version-skewed desktop/server pair would have seen a dead stream, not a compile error. **The first persistence guard was VACUOUS**: `SqliteTelemetry` serves reads from an in-memory cache, so writing and reading through one instance round-trips through a `Vec` and never executes the INSERT's column list or `row_to_turn_metrics` — it passed with the reader mutated to collapse NULL into a measured zero, the exact defect it existed to name. Reopening the database is what forces `load_all`. Both directions mutation-tested after the fix. Live-tested: 0008 applied to a populated pre-0008 database leaves existing rows intact and NULL, not 0. **2026-09-24 -- P6's rehydrate half never reached the screen, and the `VERIFIED` above never covered it.** `PondApiClient.getSessionMessages` copies fields one at a time and never copied `thinking`, so on every pond since `bc3aa9df` a reloaded conversation showed its answers without their reasoning. The replay tests in `Chat.test.tsx` mocked `getSessionMessages` itself, so they drove the store and the panel and skipped the one mapping that dropped the field. Fixed: `PondApiClient.test.ts` feeds that mapping a raw body captured from a live server, and the mapped literal `satisfies Record<keyof SessionMessage, unknown>`, so a field added to the type and left out of the mapping is a `tsc` error -- both mutation-tested. The Orin stamp is `pai_bench.py`'s reasoning-token count, which never sets `persist_thinking` or reads history back; the replay was verified 2026-09-24 on the Mac, in a browser against a scratch server with the loopback bypass off, and has not run on the Orin. |
| 3 | **Multi-agent orchestration** | [PAI-6](./06-multi-agent-orchestration.md) | **PARTIALLY VERIFIED on the Orin 2026-08-12** -- the `delegate` tool registers and is offered, and a 2B declines to call it, which is a model fact rather than a defect. The orchestrator itself IS exercised: PAI-7's reviewer reaches `Orchestrator::spawn` without a tool and a child ran. **P1, P2, P3 LANDED** (P1 `e7c9fd9a` and P2 `3306943a` 2026-08-07; P3 `9c50d867` 2026-08-09). P1 the orchestration domain and the `Orchestrator` port in `pond-core`, domain-only — the workstream's central property, *a subagent's scope is a subset of its parent's*, made STRUCTURAL rather than checked, because that rule gets written as a runtime check and deleted by a later refactor in most systems that have it: `TaskRequest` (the part a model supplies) has no scope, tool, depth or session field and is `deny_unknown_fields`, so a call carrying one is a parse error the model can see rather than a field silently dropped; `RolePersonalData`'s codomain is "the parent's scope" and "Guest", so no value widens and the profile axis needs no check; `DelegationDepth` has no `Deserialize`, no `Default` and no public constructor from a number. P2 `GooseOrchestrator` — GIAP OWNS the child loop rather than calling Goose's `run_subagent_task`, which is `pub` inside a `pub(crate) mod` and, more to the point, renders Goose's own `subagent_system.md` unconditionally, so a visibility patch would have bought a child that introduces itself as "a specialized subagent within the goose AI framework" and a shim that would not catch it. **The patch set stays at five.** P3 scope inheritance — `DelegationAuthority::for_turn` built at the edge from the turn's ACTUAL post-selection, post-guest-subtraction allow-set (the catalog would have made every downstream intersection a no-op, which is how PAI-1 P5 shipped inert), `narrow_child_groups` subtracting what the CHILD's scope denies, and the draft gate answered by WITHHOLDING rather than by checking. That last one is the move PAI-1 P5 made for guests: `engine_session_map.session_id` is the PRIMARY KEY, so a child cannot be mapped to its parent without overwriting the parent's own pairing, and a mapping that worked would hand a `personal_data: deny` child its parent's scope straight back. **The defect P3 closed is worth naming**: P1 derived the child's scope and its tool set as two independent statements, so a `Household` parent running a deny role produced a child whose scope said `Guest` while it still held `giap-memory` — whose MCP tools carry no session at all and read the household's memory regardless. A scope that says Guest while the tools say Household is a laundering route, not a narrowing. **P4 LANDED 2026-08-09** (`d45566ee`) — `context_fraction` reaches `CompactionProfile`, shrinking `history_token_budget` and only that, because scaling the resolved window would move the KV prefix and undo PAI-4 P5; plus the parent turn's own device claim, since a child does not queue behind a parent's provider call, it overwrites the one retained prefix. **P4 as first written broke invariant 3's child half and `a9a921a6` repaired it**: an inheriting child took `acquire_many_owned(0)`, which never blocks, and a device hold belongs to the SESSION — so N delegations from one parent turn ran N abreast on the one GPU. A `bool` was the wrong answer to "may this child skip the queue", because the question is *which* queue; a hold now carries its own one-permit semaphore, so a child never queues behind its own parent and always queues behind its siblings. **P5 LANDED 2026-08-10** (`c0f2bf2a`) — the `giap-orchestrator` extension and the `delegate` tool, which is the phase that made the workstream real: P1, P2 and P3 were all correct and all inert because nothing called `spawn`. The tool resolves the caller's engine session id out of `_meta` (un-forgeable — the engine strips any model-supplied value and re-inserts its own) and refuses identically on all four inputs that resolve to no live authority. `ext_orchestrator_enabled` needed its OWN `default_*` fn returning `false`; reusing `default_ext_enabled()` would have shipped delegation on for every install. `registration_matches_the_catalog.rs` ties this file's extension count, the registration list and `TOOL_GROUPS` together — an uncatalogued builtin is treated as a *user-added* MCP server that selection never narrows, which for `giap-orchestrator` would have inverted the whole intent. **P6 LANDED 2026-08-11** — subagent progress reaches the parent's stream through an unbounded `mpsc` per live turn, keyed by the parent's GIAP session id, with the parent's drain doing a biased select on the receiver. Unbounded deliberately: the sender is the child loop running *underneath* the parent's own poll, so a bounded channel's `send` would deadlock the very future that drains it. The frame carries a tool NAME and never its arguments — `child_tool_names` reads only `MessageContent::ToolRequest` and structurally cannot express anything else, which is what replaced `as_concat_text()`'s accidental protection when the loop began reading `msg.content`. **P7 and P8 LANDED 2026-08-11, and both are refusals before they are features.** P7's per-role model is inert on every `runs_on_this_device` provider and live off-device: off-device an assignment is a field in a request body, on-device it is a second GGUF into the one model slot plus a re-prefill the parent's next turn pays for. It refuses the MODEL, not the delegation, so a role authored with one is still runnable on an Orin. P8's background tasks are refused on any provider `max_concurrent_subagents` limits to 1 — keyed on that function rather than a second reading of `runs_on_this_device`, so it cannot drift from the number the semaphore enforces — and a background run keeps its OWN token owned by the parent *session*, because inheriting the turn's token would kill it when the reply ends and make it not a background run. **Both gates originally failed OPEN on an unrecognised provider string** (`1cacad80`, `39db6abb`): "not here" was being read as "elsewhere", so an unknown provider meant a role model was honoured and a background run permitted. The provider question now has a third answer and both grants require a positive claim. **PAI-6 is COMPLETE: P1-P8 LANDED.** |
| 4 | **Hard profile boundaries** | [PAI-1](./01-identity-and-profile-boundaries.md) | **VERIFIED on the Orin 2026-08-12** (pai-bench PAI-1 PASS: a guest sees no personal context and cannot claim a source). **COMPLETE — P1-P8 LANDED**, plus **P9 LANDED 2026-08-11**: the device→profile rung, which this row claimed COMPLETE while it was missing. `PushToken` was `{device_id, token, platform, updated_at}` and `PairingCode` was `{code, expires_at}` — neither carried a `profile_id`, so PAI-7 section 3.4's "profile → paired devices → push tokens" did not exist and P5 could only have broadcast a targeted proposal or delivered nothing. Migration 0043 puts `profile_id` on `devices` and `pairing_codes`, `ON DELETE SET NULL`. **The member is captured at pairing-code ISSUANCE, never from the pairing request** — `IdentificationSource::PairedDevice` outranks both face and explicit, so a client-asserted profile would outrank every proof the pond can make; both pairing-code routes refuse a non-loopback peer, so binding at issuance means the answer comes from somebody standing at the pond. NULL means unattributed, and the two directions are deliberately NOT mirror images: identity falls through to the next rung, delivery returns NO unattributed device, so a targeted proposal reaches nobody rather than every unclaimed screen in the house. **COMPLETE 2026-08-11 — both halves.** The delivery half got its caller (PAI-7's reviewer); the identity half landed the same day: `Handshake::caller_for_token` surfaces the device from the token the pond issued, `Principal` carries it, `ProvenDevice` has no constructor taking a caller-supplied string, and `resolve_turn_scope` feeds `paired_device_profile` from a `DeviceRung` where only `Member` answers `Some`. The loopback dev bypass returns before a token is read, so it carries no device — deliberate, since the alternatives are that every local request speaks as whoever last paired a phone, or that a client names itself **P10 LANDED 2026-08-12 — the member's own particulars, and who may hear them.** The prompt builder reads `preferred_name`, `birthday`, `language` and `accessibility_atypical_speech` from `profiles.preferences`; the desktop wizard collected them and wrote them to browser **localStorage**. Reader, writer and route (`PATCH /api/v1/profiles/{id}`) all existed and nothing joined them, so the assistant had never learned any household member's preferred name or birthday — the same reader-with-no-writer shape as the 22 inert switches and the registered-but-unreachable extensions. **The boundary half is the reason it is a PAI-1 row:** `profile_context_for` fell back to `primary_profile_id` for `ProfileScope::Household`, so once a writer existed, every unattributed turn would have asserted the PRIMARY member's name and birthday while somebody else was talking. Personal particulars are now `Owner`-scoped only; `atypical_speech` deliberately survives into `Household` because it is an accommodation ("be patient, interpret charitably") and not a disclosure about anyone. The scoping rule is extracted as `particulars_for(attributed, prefs)` so it is testable without an `AppState`, and five tests pin it — including `camel_case_keys_are_not_read_and_that_is_the_point`, because the UI holds these fields in camelCase and a camelCase key returns 200, populates the row, and reaches the model as nothing. Client side: `updateProfilePrefs` / `createProfile` / `getProfilePrefs` added, the wizard ensures a primary profile exists (a fresh pond had none and nothing ever created one), and `migrateLocalProfileToServer` lifts what is already stranded in browsers — never overwriting a value the pond already holds, and leaving the marker unset when there is no member yet so a later boot retries. Mutation-tested on both sides: reverting the scope, sending camelCase, booleans as booleans, localStorage winning a tie, and marking the migration done with nobody to attach to. **`language` still has NO collector anywhere** — no UI, no API caller, only test fixtures — so `Always respond in {language}` remains unreachable in production and is its own change. **2026-09-24 -- review of PR #362 found the boundary crossed by three roads this branch opened, and closed them before merge.** None of these reached main: each is a surface this branch added. (a) `GET /suggestions` mapped every non-Guest caller to `ProfileScope::Household`, whose read predicate is empty -- safe on a pond of one, where `Household` IS that member, and a disclosure on a pond of two, where an identified member resolves to `Owner(them)`: they were shown the other member's composed questions, memories, and calendar and mail counts. It reads at the resolved scope now. The composing pass also held any mix of members' notes in one prompt and attributed each question to the owner of whichever note number the MODEL wrote, so a misnumbered question about one member was queued under nobody and offered to all; `one_owners_candidates` puts one owner's notes in each call. (b) `GET /reminders` and dismiss were unscoped while the batch engine stamps each reminder with its member, so a guest read and could dismiss a member's dated reminders; both now take the caller's scope, and the engine's own promotion pass passes `Household` explicitly. (c) `resolve_turn_scope` persisted the paired-device rung with the strength-only write, from read routes too, and `PairedDevice` outranks every source -- so one member's phone opening another's conversation took it, and the batch extractor would have filed the second member's words as the first's memories. The strength-only write keeps its job ("this is Liz" must correct a wrong face match, and a test on main asserts it); the device rung goes through the new `claim_session_identity`, which binds an unattributed session or strengthens the same member's and never moves a session to someone else, inside the UPDATE. Every fix is pinned by a test that fails when it is reverted, with a control; (c)'s call site by a tripwire in `device_rung_wiring.rs`. **Verified on the Mac only** (unit, route and `scripts/live-test.sh`); the Orin status above is for main and is not re-run for these surfaces. **Still open, pre-existing on main:** `get_session_messages` and `get_session_attachment` take no `Principal`, so any paired client can read any conversation, images included, by id -- the same class as (b), found during the same week's PAI-5 fix and deliberately left for its own change. |
| 5 | **Large context**, using each model's window dynamically and to the fullest | [PAI-3](./03-context-governor.md) | **VERIFIED on the Orin 2026-08-12** (pai-bench PAI-3 PASS: a 7,568-token turn at 862 tok/s prefill, window 16,384). **P1-P4, P6 LANDED** (P3 completed by P3b 2026-08-06); **P5 code landed 2026-08-06; MEASURED ON THE ORIN 2026-08-11** — decode is a flat 30.35 tok/s at every depth, so reserving R output tokens costs `R / 30.35` seconds and the reserve is a latency budget denominated in tokens (512 tokens = 17 s). Prefill peaks at 976 tok/s near 4 096 and falls to 820 at 16 384, so a cold prefix at the real window costs ~20 s. See `docs/developer/orin-prefill-measurement.md` **THE LOOP DID NOT CHECK WHETHER IT ANSWERED — fixed 2026-08-12.** The agent loop ended when the model stopped emitting tool calls, which is not the same as the question being answered; eighteen exit paths and not one read the request after the turn started. Goose had the mechanism all along — on a turn finishing without a tool call it re-prompts "check whether the goal has been fully met" and iterates — guarded on a goal being set, and `set_goal(` had ZERO callers here, so the arm was dead. **Wiring it required a goose patch first, and that is a PAI-1 finding:** `Agent::goal` is one slot on an agent shared by four concurrent chat streams (`sse_semaphore`), so the process-wide setter would put one member's request text into another member's turn as a user message. Fork patch six adds `set_session_goal` keyed on the per-reply session id (`9cf946903`). The goal is the RAW request, never `turn_text`, which wraps it in `<system-context>` with injected memories the model would then be asked to satisfy. Measured on a Mac, gemma-4-E4B: "how old are each of the former Kenyan Presidents?" went from 4 tool calls and "unable to find" to 7 calls and the actual ages; "what time is it in the first 10 states alphabetically?" from a flat refusal to a per-state table across four time zones with Alaska honestly flagged. **Costs ~2x the inferences per turn** (E4B 2->4 and 5->9, E2B 3->6 and 2->5) because the check re-arms whenever the model does more work — 3 nudges on one turn, not 1; capping it at one would have stopped the 7-call turn around its fourth. Hence `goal_check_enabled`, default true, a SETTING and not a `ModelClass` tier: that enum's only cheap tier is `Large`, meaning served from another box, so gating on it would disable this for every on-device pond — where it was measured to help most. **Separately, the prompt was telling the model to stop:** `turn_budget_note`'s capped branch, which every default install takes (`agent_max_turns` 50; only `0` is uncapped), said "if you are running out of steps, stop gathering and answer with what you have" while the non-default branch demanded completion. Now: use as many steps as needed, and if the limit is genuinely reached, name what could not be finished and never present a partial answer as a complete one. **NOT VERIFIED ON THE ORIN** — the 2x will bite far harder there (E4B's 7-call turn took 326 s on a Mac), so the default may not survive contact with the device. **THE VOCABULARY HALF IS CLOSED 2026-08-14** and the cause was not the prompt: goose appended the check as an INVISIBLE USER MESSAGE reading `**Goal:** {goal}` -- bolded, the noun repeated around it, positioned after the system prompt, the whole tool schema and the entire history, so it was the last thing in the context before generation. The prompt's prohibition was never the trigger; it was the only counter-pressure, applied from thousands of tokens away, and `format.rs` had already recorded that a competing suggestion beats a buried one on these exact models. Fork patch seven rewords all three injections (completeness check, grind reminder, `/goal` kickoff), guarded parent-side by `pond-adapters-goose/src/goose_nudges.rs`. The prompt guard's `contains("goal")` assertion was removed only AFTER that landed -- keeping it would have required the prompt to introduce a word the harness no longer says, making the prompt the only place the model ever meets it. **2026-09-24 -- the window arithmetic this row was verified with moved into pond-core's `device_budget`**, so the goose adapter can ask whether picture support fits beside a model with the same numbers `apply_jetson_settings` sizes the window with. Pinned behaviour-identical for the five shipped Orin files (E4B-qat plus its drafter still 16,384); two inputs changed on purpose -- the drafter is charged at its catalogue size whether or not its file is on disk, and an encoder term exists but is zero on the Orin until an encoder is measured there. **The pai-bench PAI-3 re-run on the Orin after the move is OWED**; until it runs, the VERIFIED above is for the code before the move. |
| 6 | **Smart compaction** based on different models, time and cache age | [PAI-4](./04-smart-compaction.md) | **VERIFIED on the Orin 2026-08-12** (pai-bench PAI-4 PASS: ReusePrefix cut prefill 36x). **P1, P2, P3, P4, P6 LANDED 2026-08-06** (P1 `ModelClass` + strategy dispatch, domain only; P2 large-tier re-summarisation — the rolling summary rebuilt from the source messages instead of from its own previous output, gated on `ModelClass::Large` and wired into `run_compaction_pass`, so P1 now has its first consumer; P3 age-weighted retention — `compaction_verbatim_days` plus a tool-result rung between "leave it alone" and "drop the whole turn", fed by `Message::created` on the live Goose trim path and firing only when a conversation is already over budget; P4 compact-on-resume gate, wired to the session reopen; P6 `should_compact` now moves the server between turns, rate-limited by `claim_compaction`, and `reset_session` has its first production caller); **P5 code landed 2026-08-06; MEASURED ON THE ORIN 2026-08-11, first clause settled, second still open** — a cold prefix costs 4.19 s at 4 096 and 19.97 s at the pond's 16 384 window, growing faster than linearly because prefill itself degrades with depth. So recompacting when already cold is free (the prefill is owed regardless) and a needless invalidation costs ~0.1 s versus ~20 s on the same turn. The second clause — warm turns show no NEW re-prefills — needs a TTFT trace from a deployed GIAP binary and is honestly still open. See `docs/developer/orin-prefill-measurement.md` (`prefix_cache.rs`: `PrefixCacheState` + the six `InvalidationReason`s recorded in `goose_agent.rs`, `Agent::prefix_cache_state` defaulting to `None`, and the trimmer's age rung now firing on a cold prefix as well as over budget — the warm half was already P3's); **P7a (the API half) LANDED 2026-08-06** (`POST /sessions/{id}/compact` — the manual axis, running the same `run_compaction_pass` behind the same `claim_compaction` rate limiter as P6, and reporting a `status`/`reason` pair with the session's real utilisation when it refuses); **P7b (the desktop control) PARTIALLY LANDED 2026-08-06** — the bullet said "a control on the existing `ContextCard`", but `ContextCard.tsx` is the MCP-UI tool-result renderer and the shipped app had no context-pressure surface at all, so P7b is a new surface across the chat views, not a button. `context_warning` is now in the `ChatEventType` union, `PondApiClient.compactSession()` posts to the P7a endpoint, and a shared `ContextPressureNote` renders the pressure line and a "Compact now" control in `hub/views/ChatHub.tsx`. No Rust changed — the frame already reached the client, so "zero consumers" was a *rendering* fact, not a transport one. `sections/Canvas.tsx` remains a deliberate deferral. **P7b-fix LANDED 2026-08-07 (`1a4dda59`), closing both of the open defects:** the "Compact now" control could never reach `status: "compacted"` on any default install, because P6's pressure axis took the shared `claim_compaction` quota one statement after emitting the frame and strictly before the button was rendered — six pressured turns gave six `cooling_down` refusals and never one pass. `claim_manual_compaction` differs in exactly one respect, skipping the turn cooldown; it still requires the session, still recomputes `should_compact` under the same lock, and still STAMPS `turns_at_last_compaction`, so a press rations the automatic axis exactly as an automatic pass would and the two cannot double-spend the summariser. What bounds the manual axis is `SessionSummaryService::refresh` answering `NothingToDo` from the through-pointer before it reaches `provider.complete` — not the cooldown. `compaction_in_flight` is untouched and still read before the claim, which is what protects the serial on-device engine. And `sections/Chat.tsx` got the branch and the render it was blocked from, with a real render test; the `ChatHub.tsx` grep now covers both surfaces and says in the file that it is a tripwire, not coverage |
| 7 | **Personal context streaming** — on-pond, on-mobile, and internet accounts | [PAI-8](./08-personal-context-streaming.md) | **VERIFIED on the Orin 2026-08-12** (pai-bench PAI-8 PASS: a member connects a camera source and its event becomes a context item). **P1 AND P2 LANDED 2026-08-11.** The whole of P1 landed in one day in three steps that are worth keeping separate: the storage half (`b58361e1`, `a9062615`), which was **inert and stamped `DESIGNED` for a day** while I repeated that claim into two more documents without looking at the tree; the producer (`BusProducer`, bridging the event bus, with an allow-list for discrete sensor signals and a deny-list for the vision pipeline's `motion` sentinel, tied to the literal `pipeline.rs` emits); and the wiring plus `POST|GET|DELETE /context/sources`, without which `upsert_source` had no caller in the workspace and the corpus was empty by construction. **The owner is resolved, never supplied** — there is no `profile_id` field in the connect body, because every item inherits the owner and 0044 refuses to let it change afterwards; `Household` and `Guest` are refused rather than defaulted. **P2 registered `giap-context` behind `ext_context_enabled`, which ships OFF** — the toggle is what answers the cost objection: two read tools in every turn's prompt is real on a 4 096-token window where schemas are already ~88% of it, so a pond with no sources pays nothing. Count moved 16 -> 17 extensions and 62 -> 64 tools in the three places `registration_matches_the_catalog.rs` ties together, and `only_the_deliberate_extension_toggles_ship_switched_off` was renamed from `exactly_one_...` when it fired, which is the decision that guard exists to force. P3-P8 unstarted |
| 8 | **Privacy and security guardrails** to minimise data and secret exposure | [PAI-2](./02-privacy-and-security-guardrails.md) | **VERIFIED on the Orin 2026-08-12** (pai-bench PAI-2 PASS: offline mode refuses an outbound call naming the host and the mode; no secret values in GET /settings). **P0-P5, P6a, P7 LANDED** (P3, P5 partial); **P6a LANDED 2026-08-06** — five of P5's six ungated senders gated plus a sixth the guard could not see (the ~100 MB ONNX Runtime fetch, which egresses via a `curl` subprocess); `UNGATED_SENDERS` 6 → 1, `MAX_UNGATED` 6 → 1; and `set_network_mode` had ONE call site, so `network_mode = "offline"` was a silent no-op on `pond chat`, `pond setup` and (found by the synthesis pass) `pond models`. **P6b PARTIALLY LANDED 2026-08-06 (`9dd129bf`) — ONE of its three parts.** `routes.rs`'s egress sites are gated and `run_agent_cmd` installs the mode, so **`UNGATED_SENDERS` is now EMPTY and `MAX_UNGATED` is 0**. This row read a flat `LANDED 2026-08-07` for two days, which was wrong twice over — the commit is dated 2026-08-06, and PAI-2's own section 4 splits P6b into three parts of which two are still PAI-8-blocked: the draft gate for outbound connector actions, and P3's third redaction chokepoint. The second is blocked for the sharper reason that there is **no call site to wire** — PAI-8 creates the first one, which makes this a two-way dependency rather than a queue. Read the per-phase stamp, not this cell, before telling anyone P6b is done. Keep the empty list: an empty list with a zero cap is the positive claim "there is no known ungated sender", re-proved by the partition test on every run, and deleting it would let the next unclassified sender be classified by adding an entry rather than by gating the call. **P8a LANDED 2026-08-07 (`6f25e405`)** — `PolicyDecision::verdict()` returns `allow`/`would_deny`/`deny` as a FIELD rather than a log-line shape, so the enforce flip has a number to read. **P8b still BLOCKED** on a release's worth of that telemetry. **P6b's third part now has a CALL SITE as of 2026-08-11** — `main.rs` constructs an `IngestPipeline`, whose `Redactor` is not an `Option`, so redaction-before-persistence is enforced by the type at a live call site. **Its blockage was also mis-recorded:** the redaction chokepoint was recorded as circularly blocked on PAI-8, and PAI-8 P1 landed with `IngestPipeline::new` taking a `Redactor` that is deliberately not an `Option` — so the chokepoint is in the type, present and merely unreached. The remaining part, the draft gate for outbound connector actions, is blocked on a *subject* rather than a call site: there is no connector in the tree and PAI-8 puts the first at P4 |

**Every row carries TWO states, and the second one is new because the first kept lying.**
`LANDED` means the code is in and the gates in 2.3 passed. `VERIFIED` means somebody drove the
capability on real hardware and it did the thing. They are not the same claim, and on 2026-08-12
this programme found three places where the difference mattered: PAI-8 read `DESIGNED` while it was
half-built and unreachable, PAI-7 read `COMPLETE` while yielding nothing on an Orin, and
twenty-two settings switches rendered without being operable. All three passed the full test suite.

So a status cell must say both. The vocabulary is:

| word | meaning |
|---|---|
| `DESIGNED` | the document exists and its current-state claims were verified against code |
| `LANDED` | the code is in and 2.3's gates passed |
| `VERIFIED` | `scripts/pai-bench.sh` drove it on hardware and reported PASS |
| `NOT VERIFIED` | it has been run and it does NOT work, or it has never been run — say which |
| `UNVERIFIABLE` | no probe can reach it yet, with the reason |

`crates/pond-core/tests/every_pai_row_states_its_verification.rs` fails the build when a row omits
the second word. A convention nobody enforces is exactly how the three cases above survived.

They are equally weighted and mutually interdependent. `DESIGNED` means the document exists and its
current-state claims were verified against code; it does **not** mean any code has changed. `LANDED`
is stamped per phase, and means the gates in 2.3 were run and passed.

**PAI-1 is COMPLETE** as of 2026-08-05: P1-P8 all landed, including P4's policy layer and the
repair of P5, which was recorded as landed while being inert on every default install. PAI-3 P1/P2
and PAI-2 P0 are landed, PAI-2 P4 encrypts `secrets.json` at rest, and PAI-2 P1 is complete: the
mode, the identity-assertion call site, and the draft-decision gate that gives a staged action an
owner. **PAI-2 P5 landed 2026-08-05**: `network_mode` (`open`/`allowlist`/`offline`) now refuses an
outbound call before the socket opens, at five of the eighteen HTTP-sending files in the tree;
**P6a extended that to all but one file on 2026-08-06.**

**PAI-6 is COMPLETE as of 2026-08-11. PAI-7 is CODE-complete and does not work on hardware -- read its row before repeating that it is done.** Every one of the eight
now has landed, reachable code.** PAI-1 gained its identity rung (P9's second half), PAI-5 P5 landed,
and PAI-8 P1 went from inert to reachable in a day. PAI-1 is complete and
gained P9; PAI-2 runs through P8a; PAI-3, PAI-4, PAI-5, PAI-6 and PAI-7 are landed. **All eight
workstreams now have landed code**, PAI-8 included — it gained P1 and P2 on 2026-08-11.

**Corrected the same day, and worth keeping as a worked example rather than tidied away.** The three
sentences that stood here said PAI-8 was the only workstream still design-only. That was false when
written: three PAI-8 commits had landed hours earlier and the row in section 1 had not been updated,
so I read the row instead of the tree and then propagated it into the master roadmap as well. Two
things this says. First, the rule in 2.1 item 3 — re-verify the current-state claim you are about to
rely on — applies hardest to *this file*, because a status ledger is the one document everybody
trusts without checking. Second, a claim about absence needs an executable guard, not a cell: PAI-7
P3a shipped with one and it fired correctly; PAI-8 shipped without one and the gap became a
documentation error within a day. It has one now.

The dependency order that forced this sequence is now paid off. PAI-7 P4 was blocked on PAI-6 having
an orchestrator a caller could reach, and it does; P4 then unblocked P5, whose targeted-delivery path
had been functional and unreached, and P7, whose ledger had no reader. **PAI-8's P8 is no longer
blocked either** — it needed PAI-7 to have a proposer to wire an ingest event into, and PAI-7 P3 has
one that P4's loop actually runs.

**What is left is wiring, one decision, and two measurements.** The wiring is PAI-8's: registering
`giap-context` (which moves the tool count in three places that a test ties together), constructing
the repository, and giving the ingest pipeline an on-pond producer. **The "circular" dependency with
PAI-2 P6b turns out to be one-way in practice** — P6b's third part needs a redaction call site that
only PAI-8 can create, and PAI-8's side is not blocked on P6b at all. `IngestPipeline::new` takes a
`Redactor` that is deliberately *not* an `Option`, so the chokepoint exists in the type the moment
anything constructs a pipeline. There is nothing to decide about the order; there is work to do on
one side of it. The decision that does remain is P6b's OTHER part, the draft gate for outbound
connector actions, which cannot be designed until a connector exists (PAI-8 P4 at the earliest). The measurements are PAI-7's — a completed proactive
review observed on an Orin, and a targeted proposal queued for a device that was switched off and
delivered on reconnect — plus PAI-4 P5's second clause, which wants a TTFT trace from a deployed
binary rather than from a probe.

**Two things are worth carrying forward from how this went, because both cost real defects.** Nearly
every genuine bug found in this stretch was found by re-applying a mutation and watching the suite
stay green, not by reading code: a phase that landed a budget correctly and broke invariant 3 in the
same pass; a repair that fixed two HIGH bugs and guarded them with tripwires that could not see
either; a type built to make a clamp unskippable that shipped with a `pub(super)` field and its
clamp uncalled, after its own doc comment warned against exactly that. And `rustc` was right twice
while being read past — a `dead_code` warning naming the uncalled clamp sat in output that was
scrolled through for test totals. Note it **cannot** fire for `pub` items in a library crate, so its
absence proves nothing; that blind spot is what let PAI-7 P3's proposal domain look wired. Two of the eight are code-landed with their own deciding
measurement outstanding — PAI-3 P5 and PAI-4 P5, see the verification-debt note in section 4 — and
neither may be promoted to `LANDED` without an Orin.

Read `UNGATED_SENDERS` in `crates/pond-core/tests/egress_guard.rs` before you tell anyone
`network_mode = "offline"` means offline. **As of P6b (2026-08-07) the list is EMPTY and the cap is
0** — every sender the guard can see reaches the tracker. The cap only moves down; it has gone
6 → 1 → 0 and must never go back up. Two things the list cannot tell you, both found by gating the
rest: the guard finds senders by looking for `reqwest`, so it was blind to the ~100 MB ONNX Runtime
download that shells out to `curl` (now gated; it is the only subprocess sender in the tree, so the
class is closed but the blindness is not); and a gate is worth nothing if `set_network_mode` never
ran, which was true of three of the four downloading entry points. **An empty list is not the same
claim as "nothing can egress"** — it says every sender the guard can SEE is gated. Ask what the
guard can see, not what it lists. Note
also that the guard classifies FILES, not crates: the crate-level rule the design asked for ("every
`reqwest`-using crate references `record_egress`") would have failed for eight of eleven crates on
the day it landed, and most of what it flagged was a health probe on 127.0.0.1.

One consequence of P4 worth knowing before you go looking for it: `giap.sh doctor` now FAILs on
every pond that has not yet been restarted on a P4 binary, because the store on those really is
plaintext. That is the check working. Do not downgrade it to a warning.

---

## 2. The recursive check

Run this every time, in order. It is short on purpose — a checklist nobody completes is worse than
none.

### 2.1 Before starting work on any PAI item

1. **Re-read this file and the [master roadmap](../personal-agentic-intelligence.md)**, then the
   specific PAI document. The roadmap's dependency graph decides whether this item is even eligible
   yet.
2. **Check the prerequisites are LANDED, not merely DESIGNED.** PAI-6 with PAI-3 unlanded gives
   every subagent a context budget derived from a number four code paths disagree about.
3. **Re-verify the current-state claims you are about to rely on.** Every document carries a
   verification date. Line numbers rot faster than prose — grep for the symbol, never trust the
   `file:line`. If a claim is now false, fix the document in the same change; a design doc that lies
   is worse than no design doc.

   Two failure modes seen in practice, both mine: **counted claims** go wrong when you grep for a
   pattern rather than the thing itself (the extension count was wrong twice because one
   registration uses a const, not a string literal), and **a correction that makes a discrepancy
   vanish** deserves more suspicion than one that creates work.
4. **Re-read the seven cross-cutting invariants** in the master roadmap section 3. They bind all
   eight workstreams.

### 2.2 While working

5. **Ask what this change does to the other seven.** That is what "interdependent" means in
   practice, and it is the check most likely to be skipped:
   - Does it move the **KV prefix**? (invariant 1 — measured 3.7 s re-prefill on the Orin)
   - Does it add tokens to the **preamble** rather than the working set? (PAI-3's asymmetry rule)
   - Does it introduce a path where `profile_id` can be `None`? (PAI-1's entire lesson)
   - Does it create a new **egress** point without `record_egress`? (invariant 4)
   - Does it put a **secret** on `Settings`? (PAI-2 — `GET /settings` serialises the whole struct)
   - Does it let a **`Guest`** session reach personal data? (PAI-1, PAI-8)
   - Does it make anything **block a turn on an LLM call**? (invariant 2)
   - Does it perform a side-effecting action **without approval**? (PAI-7's propose-do-not-act rule)
6. **Keep policy in `pond-core`, mechanism in adapters** (invariant 5). If it would need rewriting
   when Goose is swapped, it is in the wrong crate.

### 2.3 Before stopping

7. **Stamp the phase** `LANDED` or `DEFERRED` in its PAI document, matching the convention in
   `context-and-reasoning-roadmap.md`. A deferral is written down with its reason and its cost, not
   silently dropped.
8. **Update the status column** in section 1 above and in the roadmap's table.
9. **Update section 4 below** — the open questions and running notes.
10. **Run the gates.** `cargo fmt --check`, then the fast-crate `cargo clippy` / `cargo test` set
    from `.github/workflows/ci.yml`, then `cargo check -p pond-server -p pond-adapters-goose` for
    anything touching the live path. Note the submodule caveat in section 3.
11. **Run the live server** (section 2.4). Not optional for anything that changes a migration, a
    route, a handler or startup wiring.
11b. **Drive the capability on hardware** — `scripts/pai-bench.sh` (add `--slow` for PAI-7), and on
    the Orin rather than a laptop where it matters. Then write the verification word into the status
    cell. This gate exists because gates 10 and 11 were both green on a PAI-7 that produces nothing
    and a settings panel whose switches were not switches. Green tests and a healthy server are
    evidence that the code runs, never that the capability works. A `SKIP` from the benchmark is not
    a pass — it means the probe never exercised it, and the cell says `NOT VERIFIED`.
12. **Clear the documentation debt** attached to the workstream (roadmap section 4) while you are
    in the file. It is one line each and it never gets cheaper.

### 2.4 Live server verification

Green unit tests are not evidence that the pond starts. Every test in this repo runs against a
database built by applying every migration to an empty file, in one process, with the adapter under
test constructed by hand. None of that exercises startup ordering, migration application against a
database that already has rows, route registration, the auth middleware, or the wiring in
`main.rs` — and those are where the last several defects in this programme actually were.

Run this for any change touching a **migration, a route, a handler, or startup wiring**:

1. **Build and start it against a scratch data dir**, so the run cannot touch a real pond:
   ```bash
   POND_DATA_DIR=/tmp/pond-live cargo run -p pond-server -- serve --port 4000
   ```
   Read the port it actually bound from `$POND_DATA_DIR/.runtime_api_port` — `--port` is a request,
   and the fallback walks `4000..4009`.
2. **Confirm the migration applied to a real file**, not just to a fresh in-memory fixture:
   ```bash
   sqlite3 "$POND_DATA_DIR/pond_system.db" "SELECT version, description, success FROM _sqlx_migrations ORDER BY version DESC LIMIT 3;"
   ```
   Then re-run the server against the **same** directory. A migration that only works on an empty
   database is a migration that only works once, and every install after the first is an upgrade.
3. **Exercise the routes you touched with real HTTP**, including the failure cases. A handler that
   compiles and a handler that returns the right status for a missing row are different claims.
4. **Read the logs, and read them for more than your own feature.** `WARN` and `ERROR` lines that
   were already there are still findings. Two of this programme's real defects were visible in
   startup output long before anyone looked.
   ```bash
   grep -iE 'error|warn|panic|failed' "$POND_DATA_DIR"/logs/*.log | grep -v <known-benign>
   ```
5. **Correct what the logs show**, in the same change. A log line you decided to ignore gets written
   down in section 4 with the reason, or it will be rediscovered as new.

`serve` shuts down on stdin EOF when detached, so background it with stdin held open (`sleep
infinity | cargo run ... &`) or it exits immediately and looks like a crash.

---

## 3. Known environment traps

- **The Goose submodule is uninitialized in a fresh clone.** `cargo test -p pond-core` fails at
  *manifest load*, before compiling anything, because the workspace path-depends on
  `goose/crates/goose/Cargo.toml`. This is not a regression and not something a code change caused.
  Fix with `git submodule update --init --recursive`; CI sidesteps it by cloning the fork branch tip
  directly (`.github/workflows/ci.yml:52-55`). Any Goose-side claim in these documents was verified
  against a separate checkout of `jarida-io/Goose:main` and is labelled as such.
- **`.cargo/config.toml` sets `-C target-cpu=native`.** Override it for any cross or containerised
  build or the binary can SIGILL on the target.
- **No emojis anywhere, including comments** — `no-emoji.test.ts` scans source and will fail the
  build.
- **`pond-server` needs ALSA headers, and a stale apt index looks like "no apt access".**
  `pond-server` depends unconditionally on `cpal`, so `alsa-sys` must find `libasound2-dev`; without
  it the production-binary gate cannot run at all, and neither can `pond-voice`, `pond-audio`,
  `pond-adapters-whisper` or `pond-adapters-piper`. A container may ship the runtime `libasound.so.2`
  and not the headers. `apt-get install libasound2-dev` on a fresh container 404s on every mirror,
  which reads as a sandbox restriction; it is a stale index. **Run `apt-get update` first.** I
  recorded "no apt access" in a commit message on the strength of the 404 alone and it was wrong.
- **A stray `pond-server` on port 4000 silently hijacks `scripts/live-test.sh`.** Check before
  running it: `lsof -nP -iTCP:4000-4009 -sTCP:LISTEN`. A long-lived `serve --native` with no
  `POND_DATA_DIR` runs against the *real* data directory, and the script used to assume port 4000 —
  so every assertion, and both onboarding writes, went to that pond instead of the scratch one. The
  script now reads `.runtime_api_port`, fails hard when it is absent, and refuses to drive a
  listener whose pid it did not start. See the 2026-08-05 entry in section 4.
- **Disk is a fixed allowance and the failure mode is disguised.** `cargo test -p pond-api` builds
  seventeen integration binaries at ~600 MB each, because every one links Goose statically. That
  alone exceeds the allowance. The symptom is
  `collect2: fatal error: ld terminated with signal 7 [Bus error]` or `No space left on device` from
  a random dependency — both read as a code fault or a broken toolchain. Run the targets one at a
  time with `cargo test -p pond-api --test <name>`, deleting `target/debug/deps/<name>-*` between
  runs. `df` shows low "Used" with zero "Avail" in this state; that is the allowance, not the disk.

---

## 4. Running notes and open questions

Append here as work proceeds. Dated entries, newest last.

**2026-08-03 — programme designed, nothing implemented.**

- All nine documents written and cross-checked: every referenced path resolves, every relative link
  works, no forward-pointing prerequisites.
- The single highest-value fix identified across the whole programme is one line: `trim_goose_history`
  resolves the context window from `std::env::var("GOOSE_CONTEXT_LIMIT")` with a hardcoded 8192
  fallback (`goose_agent.rs:1478-1481`), so a correctly configured 3K Jetson can be trimming against
  a window that does not exist. It belongs to PAI-3 P1.
- **Open:** PAI-2 lands the policy layer in `audit` mode, not `enforce`. Someone has to decide, from
  real audit logs, when to flip it. A permissions matrix written from first principles will be wrong
  in ways only real traffic reveals, and an authorisation regression in a home assistant looks like
  the lights not turning on.
- **Open:** PAI-1 phase P1 is specified as a behaviour-identical refactor. If it is not — if any
  existing single-user install sees different memory recall after it — the phase is wrong and should
  be reverted rather than patched forward.
- **Open:** the whole programme assumes one household per pond. PAI-7 states it as a deferral. If
  that assumption ever breaks, PAI-1's `ProfileScope::Household` is the type that has to change, and
  it is load-bearing for six of the eight.

**2026-08-03 (later) — documentation debt cleared; still no feature code.**

All six rows of the roadmap's documentation-debt table are done, each re-verified against code
first. Corrected: `token_tracking.md` (real provider usage is the primary path, chars/4 is only the
fallback when a provider emits no `Usage` events), `scheduling.md` (three `TaskKind` variants and
twelve MCP tools, not two and seven), `api.md` (handshake token validation is real — SHA-256 lookup
with revocation and expiry), `model_capabilities.md` (the sixth field, `tool_calling`), and
`security/ports/policy.rs` (the loopback bypass is off by default behind `POND_DEV_ALLOW_LOOPBACK`
since #94, not unconditional).

**One of the six was my own error, not the repo's.** I had recorded that `AGENTS.md` understated the
extension count at 14 while `giap_registration.rs` registered 15. Recounting gives exactly **14**
`register_builtin_extension` calls, so `AGENTS.md` was right all along. The roadmap row is corrected
and PAI-6 section 3.6 now says `giap-orchestrator` would make **15**, not 16. Worth noting as a
warning: that claim was wrong in the first commit of a document whose whole value is being accurate,
which is exactly why check 2.1.3 exists. Re-verify; do not trust a prior pass, including mine.

**The build baseline is unblocked.** `goose/` can be populated from a clone of `jarida-io/Goose:main`
when the submodule is uninitialized, which is what CI does. With it in place `cargo fmt --check`
passes and `cargo test -p pond-core --lib` runs. Do this before assuming a cargo failure is yours.

**2026-08-03 (evening) — PAI-3 P1 started. STOPPED MID-PHASE; read this before resuming.**

### Done and committed

`ContextGovernor` in `models/services/context/context_governor.rs`, exported through
`context/mod.rs` and the compatibility re-export in `models/services/mod.rs`. Types:
`WindowResolution { tokens, source }`, `WindowSource` (five rungs, with `label()` and `is_exact()`),
`EngineWindow { tokens, model }`, `ContextInputs`. Plus `prompt_window()`, which is the existing
local 8192 prompt-side clamp moved in unchanged.

**Domain only — no call site repointed, so behaviour is unchanged.** 13 unit tests;
`cargo test -p pond-core --lib` 704 passed / 0 failed; `cargo fmt --check` clean.

### The exact remaining work for P1

Four call sites, all in `crates/pond-adapters-goose/src/goose_agent.rs`. Verified 2026-08-03:

| Line | What it does now | What it should become |
|---|---|---|
| ~636 | `env GOOSE_CONTEXT_LIMIT` else 8192, in the session-hydration replay path | `ContextGovernor::resolve` with session scope; `engine_reported` available here |
| ~1478 | `env GOOSE_CONTEXT_LIMIT` else 8192, in `trim_goose_history` | same — **this is the bug the whole phase exists to fix** |
| ~907 | `effective_context_window` → `resolve_context_window` (pinned > override > heuristic) | delegate to the governor, passing `registry_pinned` |
| ~2182, ~2366 | `prompt_budget_ctx(provider, effective_context_window(...))` | `ContextGovernor::prompt_window(provider, resolve(...).tokens)` |

Also: `pond-agent/src/agent.rs:375` (`min(override, caps)`) and `pond-api/src/routes.rs:1674-1685`
(`TurnStats > override > caps`) are the other two of the four disagreeing paths named in the design.

### Three things to be careful about

1. **`GOOSE_CONTEXT_LIMIT` is still written and must stay written.** `goose_env_knobs`
   (`goose_agent.rs:131`) sets it to the raw resolved window, and it flows into Ollama's
   `options.num_ctx` — so the reported limit, the request's `num_ctx` and the KV cache agree.
   Deleting the *reads* is the goal; deleting the *write* would desynchronise them. Two adapter
   tests assert the written value (`goose_agent.rs:4224,4239`).
2. **The env var carries the raw window, not the prompt budget.** Sites ~636 and ~1478 feed
   `CompactionProfile::from_context_window` directly with it, while ~2182 and ~2366 clamp through
   `prompt_budget_ctx` first. That asymmetry is deliberate and already correct; preserve it when
   repointing, or the preamble silently grows on local providers.
3. **Wire `engine_reported` only where the value is session-scoped and model-matched.** For the
   settings path (`apply_goose_env_knobs`) pass `None` — it is process-wide, and a session's last
   turn is not evidence about it. This keeps that path behaviour-identical.

### Verification P1 still owes

The regression test from the design doc section 7: assert no file under `models/services/context/`
and no adapter budget path calls `std::env::var`. The module-level half of that already exists
inside `context_governor.rs`; the adapter half cannot be written until the two reads are gone.

`cargo check -p pond-adapters-goose` is the gate for all of the above and is slow from cold —
start it early.

**2026-08-04 — PAI-3 P1 LANDED.**

All four paths repointed; `prompt_budget_ctx` deleted from the adapter. Gates: `cargo fmt --check`
clean, `pond-core` 707 passed, `pond-adapters-goose` 100 passed / 1 ignored, `cargo check` clean on
`pond-api` and `pond-agent`.

Corrections to what the previous entry assumed:

- There were **two** env reads, not one — `trim_goose_history` and session hydration.
- The replacement is an adapter-owned `last_window` field written by `apply_goose_env_knobs`
  *before* its signature guard returns early, not a settings load per turn. A settings load remains
  only as the cold path, for a session hydrated before any turn has configured a provider.
- `routes.rs` fell back to live `agent.capabilities()`, which is better data than the name
  heuristic. `ContextInputs::capability_window` exists so that path keeps its accuracy; precedence
  there is unchanged (engine > override > caps).
- `pond-agent` used `min(override, caps)` and now lets the override win. A real behaviour change,
  taken deliberately because the crate is quarantined (Q2-05) and a fourth divergent precedence
  would defeat the phase.

**Two process lessons worth keeping.**

Adding a field to `ContextInputs` broke an existing literal with `E0063`. That is the design
working: every field is a precedence decision, so do **not** reach for `..Default::default()` at
construction sites — the compile error is the review.

A backgrounded `cargo ... | tail -N` reports the **exit code of `tail`**, so a run that failed to
compile was reported as success. Never trust the status of a piped cargo run; capture per-command
exit codes or read the output.

**2026-08-04 — PAI-3 P2 LANDED, and a four-way audit of all eight documents.**

P2: `TokenCounter` port, `HeuristicTokenCounter`, tiktoken-backed adapter on the live path.
**Exactness was not achievable** — the design assumed a GGUF tokenizer would be reachable and it is
not (private module in the fork; `pond-inference`'s belongs to the quarantined agent and would
double-load the model). Both counters report `is_exact() == false` and the overshoot-feedback
correction stays load-bearing. Gates: fmt clean, pond-core 710, adapter 103, `pond-api` and
`pond-agent` check clean.

I then ran four parallel read-only agents over all eight design docs, one pair each. Worth repeating
before any future phase — it found more than the phase itself did.

**The security finding, which outranks everything else in this programme:** `is_public_route` is
path-only while its entries read as method-scoped, and `public_routes.merge(protected_routes)` puts
every route behind that single check. `GET /settings` returns all API keys, and
`DELETE /profiles/{id}` deletes a household member, **with no token**. Now PAI-2 P0.

**I had corrected a correct claim into a wrong one.** The extension count is **15**, not 14 — two
agents found this independently. `giap-toolkit` registers through the `TOOLKIT_EXTENSION` const, so
my `grep -oE '"giap-[a-z-]+"'` could not see it, and I "fixed" the roadmap in the wrong direction
with confidence. Tool count is **61**, not 57. Lesson: **count call sites, not string literals**,
and be most suspicious of a correction that makes a discrepancy disappear.

Other corrections applied: PAI-5's prompt/engine thinking inconsistency is **already fixed** (its P3
is a no-op); `GOOSE_AUTO_COMPACT_THRESHOLD` is not local-gated, only the tool-pair knob is; there is
**no pairing lockout** and its absence is deliberate (a lockout would be a guest-triggerable DoS);
`Profile` has six fields not five; `ModelRecord.context_length` *is* written for the curated GGUF
catalog (and only there — see the 2026-08-06 entry: the correction was right and still incomplete,
because it stopped at "is written" without asking *by which providers*); PAI-8 undercounted the multipart upload routes (`/voice/calibrate` and
`/sessions/{id}/identify-user` also take uploads), which matters because PAI-1 and PAI-2 lean on
that absence argument.

**Line-number rot is systemic**, and PAI-3 caused some of it by editing the very files the docs
cite. Prefer symbol names over `file:line` when writing these documents.

**2026-08-04 — PAI-1 P1 and P2 LANDED.**

P1: `ProfileScope { Owner | Household | Guest }`, and `MemoryRepository`'s five search methods now
take `&ProfileScope` instead of `Option<&str>`. Every call site passes `Household`, whose SQL is
byte-identical to the old `None` branch, so nothing changed behaviourally — which was the point.

P2: migration `0037_session_identification.sql`, `SessionIdentity` / `IdentificationSource`, two new
`SessionStorage` methods, and `AppState.session_user_bindings` deleted with its three handlers
repointed at the column.

**Three things the design doc got wrong, all found by code rather than by reading.**

1. **`sessions.profile_id` already existed** — since migration `0003`, unwritten and unread for
   thirty-four migrations. A dead column. The design called for adding it. P2 shrank to wiring.
2. **Wiring it would have broken member deletion.** The 0003 column has no `ON DELETE` action, and
   `Database::init` sets `PRAGMA foreign_keys = ON`. That is harmless only while the column is
   always NULL. `0037` carries a `BEFORE DELETE` trigger standing in for the `ON DELETE SET NULL`
   SQLite will not let us add in place, and a test proves deleting a member with a live session now
   succeeds. **Nothing in the design pass predicted this.** The pattern worth generalising: a
   column nothing writes has no observable constraints, so its declaration has never been tested.
   Wiring a dead column is not a no-op — it activates whatever was declared around it.
3. **Three of the four `profile_id` foreign keys already cascade on delete** -- `memory_fragments`
   (`0005`), `face_embeddings` (`0013`), `face_profile_thresholds` (`0014`). `sessions` was the only
   one declared without an action. Most of P7 turned out to be built; its real job is per-category
   counts. The audit table is in PAI-1 section 3.7.

**P2 deliberately ships no `SessionIdentity -> ProfileScope` conversion.** Every session in every
existing pond is unattributed, so the method would have to answer "what scope is an unidentified
session" today, and the only behaviour-preserving answer — `Household` — is exactly the
scope-widening default invariant 2 calls a bug. P3 decides it with the paired-device, explicit and
face inputs in hand. Shipping a default now would mean un-shipping it later.

**Two defects found by reading my own diff, not by any test.** Both were in code that compiled and
passed everything:

- The identify handler read the existing binding with `unwrap_or_else(|_| unknown())`. A failed read
  would then look like "nobody is bound", letting a weak face match take over a paired-device
  session -- the exact downgrade `supersedes` was written to refuse. **A fallback default on an
  authorisation input is a widening default**, and invariant 2 says access narrows on failure. Fixed
  to refuse the write.
- `set_session_identity` was updating `updated_at`. `list_sessions` orders by it, so a camera
  recognising somebody would have reordered the user's chat history with no message sent. Metadata
  about a session is not activity in it. Fixed, and pinned with a test.

Neither was reachable by the tests I had written, because both are about what happens on a path the
tests do not take. Worth budgeting review time for the diff itself, separately from the gates.

**The live server run, which is now a standing gate (section 2.4).** Built `pond-server`, started it
against a scratch data dir, and drove the routes with `curl`. All of it passed:

- Migration `0037` applied to a real file, `success = 1`, and applied **once** across two starts.
  Both provenance columns and the trigger are present in `sqlite_master`.
- `GET /sessions/{id}/user` reports `profile_id: null, identification_source: "unknown"` for an
  unidentified session; a bound session reports its owner, `face`, and the confidence.
- `DELETE` on an unknown session is 404; `GET` on one is 200 and says nobody. The asymmetry is
  deliberate and it now holds over HTTP, not just in a unit test.
- **`DELETE /profiles/{id}` returned 204 with a session bound to that member**, and the session
  survived with its attribution released. That is the foreign-key trap from finding 2, fixed and
  proven live rather than argued.
- A binding written before a restart was still there after it. The deleted in-memory map could not
  have done that, which is the whole point of P2.

**Two false passes in my own check script, caught by reading its output.** The first run reported
PASS for `body.get("profile_id") is None` — against an `onboarding_required` error body, where every
lookup returns None. **A check that passes because the request failed reports the opposite of the
truth.** The script now asserts the status code first and only then any body predicate. Same class
of error as the vacuous tiktoken tests in the PAI-3 P2 entry; it is worth assuming I have written
one every time.

**And the live run reproduced PAI-2's P0 from a running server**, with the loopback bypass off and
no token: `GET /settings` returned API keys in plaintext, `GET /profiles` listed the household, and
`DELETE /profiles/{id}` returned 204. Meanwhile `/sessions`, `/devices` and `/memory` correctly
returned 401 — so the auth layer works and the defect is exactly the path-only allowlist match. Full
table in PAI-2's P0 entry. This is what section 2.4 exists for: the defect was in the code the whole
time, and one `curl` found it in seconds.

Startup logs across three runs held two WARNs, both environmental rather than defects: the embedding
model download is blocked by this container's proxy (403), and `auto_download` skips
`llamafile/mock` because my own live-check onboarding set that as the chat model. Recorded so the
next run does not rediscover them as new.

**The environment claim I got wrong.** I recorded "this container has no apt access" in a commit
message, on the strength of `apt-get install libasound2-dev` 404ing. It was a stale index;
`apt-get update` fixed it in one command, and `pond-server` builds. Section 3 now records both this
and the disk-allowance trap, which disguises itself as a linker bus error.

**2026-08-04 (later) — PAI-1 P3 resolver landed; the chain has a missing rung.**

`identity_resolution::resolve` is in, six tests, no call site yet — same shape as PAI-3 P1, domain
first so the wiring is small.

**Nothing links a paired device to a household member.** `session_tokens`, `push_tokens`,
`pairing_codes` and `handshake_challenges` all lack a profile column; the pairing flow never asks
who is pairing. That is the *strongest* rung of the designed chain and it does not exist. The input
is honoured and fed `None` by everyone.

**This is not only PAI-1's problem.** PAI-7 assumes it can address a notification to "the profile's
devices" and gives that as the first real producer for the dormant targeted-notification path. It
cannot, for the same reason. Capturing a member at pairing time unblocks both, and it should
probably be its own small phase rather than buried in either.

I did not fall back to `settings.primary_profile_id`. It would have made the chain look complete and
attributed every phone in the house to one person.

**One design decision worth arguing with.** An unidentified speaker resolves to `Household` in a
one-member pond and `Guest` in a shared one. The two scopes only differ when there is somebody to be
excluded from, so this is not a weakening — but it does mean adding a second household member
silently changes what an unidentified voice can reach. If that should instead be an explicit setting,
now is the time to say so, before P4 builds enforcement on top of it.

**2026-08-04 (later still) — PAI-1 P3 wired, P7 and P8 landed. Five parallel recon agents.**

I ran five read-only Explore agents in parallel over P3/P4/P5/P6/P7+P8 and then implemented
serially. **Parallel implementation agents are not viable here** and it is worth writing down why:
each needs its own cargo target dir, which means a fresh ~20 GB Goose build against a 2 GB
allowance, and sharing one target dir just serialises them on the cargo lock. The win from
multi-agent in this repo is read-only fan-out, not concurrent writes. Same conclusion PAI-6 reaches
about on-device subagents: the benefit is isolation, not wall-clock.

The recon paid for itself three times over:

- **`ProfileContext` reaches the model nowhere.** `routes.rs` binds the built prompt as
  `let mut _system_prompt` -- underscore-prefixed, deliberately unused -- and `goose_agent.rs`
  passes `None` for the profile with a TODO. So this checklist's own claim that
  `settings.primary_profile_id` "reaches the prompt" was **stale on both engine paths**. P6 is
  therefore "wire it at all", not "switch its source". Corrected in PAI-1 section 1.5.
- **P7 cannot cascade drafts or schedules.** `drafts` has no owner column (keyed by `session_id`, no
  FK) and **there is no `schedules` table at all** -- the scheduler is in-process tokio-cron.
  *(Drafts half fixed 2026-08-05 by PAI-2 P1, migration 0038 + a `BEFORE DELETE ON profiles`
  trigger. Schedules half still true.)*
- **`approve_draft` has no ownership check whatsoever.** Any session can approve any draft id. That
  is a live authorisation defect, not a Guest-degradation gap, and it belongs to PAI-2 rather than
  here. *(CLOSED 2026-08-05 by PAI-2 P1 -- see that document's P1 entry.)*

**The compiler found what a careful read-only sweep did not.** Making `AgentRequest.profile_scope`
non-optional broke twelve construction sites; the recon inventory listed eleven. `delegation.rs`
was the twelfth. Fan-out recon is good at mapping and still not a substitute for a type that
refuses to compile.

**Two more vacuous tests, again mine.** One P8 test used profile ids the fixture never creates, so
its "owner sees the shared row" assertion passed against an empty result set. Its sibling failed
loudly and exposed it. That is three vacuous-test incidents in this programme; the pattern is always
an assertion that holds trivially when the setup is wrong. Assert the *positive* case too --
"alice owns at least one row" -- not only the boundary.

**2026-08-04 (evening) — an audit agent found the defect the whole workstream turned on.**

I ran two read-only agents: one adversarial review of the five landed commits, one sweep for client
breakage. The review found something that invalidated a claim I had been making all session.

**`ProfileScope::Owner` was a no-op in production.** Every memory write path set `profile_id: None`,
so `scope_sql`'s `Owner(id)` predicate matched exactly the same rows as `Household`. Only `Guest`
restricted anything. All the read-side scoping in P1 and P3 was real plumbing with nothing flowing
through it -- resolve a session to Liz, ask what Jerry said, and you would get it.

**No test caught it, and the reason generalises.** The fixtures that produce an owned row set
`profile_id` by hand -- a state no production code path could reach. A test whose *fixture* is
unreachable tests a system that does not exist. Worth adding to 2.2: ask not only "does this assert
the right thing" but "could production ever produce this input".

Fixed: extraction stamps the owner from the turn's scope and refuses to write for a `Guest`. Three
tests assert it end to end, including that a `Household` turn stays unattributed so shared context
survives a member's deletion.

**Other findings acted on:** `POST /chat` never resolved a scope at all, so the same speaker got
different answers from `/chat` and `/chat/stream` -- and my comment there wrongly blamed voice.
`delete_profile` used `unwrap_or_default()` on the settings read, so a read failure meant the
dangling `primary_profile_id` was never cleared and the delete proceeded anyway; both failure paths
now abort. Four vacuous tests removed or strengthened, including one asserting that a function whose
body is `ProfileScope::Household` returns `ProfileScope::Household`.

**Findings recorded but NOT fixed**, both written into PAI-1's phase list:

- The `giap-memory` MCP tools bypass the scope entirely -- including `forget_memory`, which deletes.
  A guest gets no memories injected and can still have the model recall or destroy everything.
- A real TOCTOU between `PUT /sessions/{id}/user` and `POST /identify-user`: both read `Unknown`,
  both pass `supersedes`, and the later write wins regardless of rank. My comment claimed the only
  loser was a competing face match on the same frame. That understated it.

**The SIGILL landmine is real and I hit it.** `pond-api`'s test binary died with `signal: 4, SIGILL`
after a rebuild -- the `target-cpu=native` artifact problem AGENTS.md documents for CI. `RUSTFLAGS=""`
fixes it and is now what I use for every gate, matching `ci.yml`. It costs a one-time rebuild of the
goose rlib. Do not diagnose this as a code fault.

**Client breakage check: none.** `pond-desktop` never calls profile-delete or any of the
`/sessions/{id}/user` routes, and its API client already handles both 204 and 200-with-body. The
breakage is documentation only -- `docs/api.md` still specifies `204 no body` for profile delete and
documents none of the four session-identity routes.

**2026-08-04 (late) — PAI-1's two named holes closed.**

**The guest-reachable memory tools.** Closed one layer above where the audit found them.
`recall_memories` and `forget_memory` carry no session of their own and `MemoryMcpServer` is
process-global, so there is nowhere inside them to check. Instead a `Guest` session is never given
`giap-memory` (or draft, audit, vision, sensors) at all -- subtracted **after** `select_groups`,
since those groups are `core` and selection puts them back unconditionally.

Worth remembering as a shape: **when the thing you want to check has no identity, move the check to
the layer that hands it out.**

I did not thread a scope into the MCP server per turn, which would additionally scope an *Owner*'s
tool calls. The only mechanism available is `set_current_session_id`, a process-global
`RwLock<String>` that the SSE semaphore already permits more than one turn to race on. Using it
would trade a guest hole for a misattribution bug, which is the worse of the two.

**"The only mechanism available" was wrong, and PAI-2 P1 found the other one on 2026-08-05.** Goose
stamps `agent-session-id` into every `CallToolRequest`'s `Meta`; rmcp serialises it as the wire
`_meta` and hands it to the tool handler in `RequestContext.meta`. That is per-call and race-free,
needs no Goose patch, and is what `giap-draft` now resolves its speaker from
(`crates/pond-mcp-server/src/session_meta.rs`). `giap-memory` and `giap-toolkit` still read the
raced global; adopting the reader is the follow-up. The lesson generalises: **"there is nowhere to
hang the identity" is a claim about the transport, and transports carry more than the parameters
you were looking at.**

**The TOCTOU.** `set_session_identity_if_stronger` does the rank comparison inside the `UPDATE`,
building the `CASE` from `IdentificationSource::ALL_RANKED` so the ordering stays domain policy. A
test pins `ALL_RANKED` against `rank()` in both directions -- if they ever drift, the conditional
write would enforce one ordering while every in-memory check enforced another, and the disagreement
would only ever show up as an occasional unreproducible downgrade.

Zero rows updated is ambiguous between "no such session" and "not superseded", so the adapter
disambiguates with a follow-up count rather than guessing. One is a 404, the other a normal refusal.

**Two disk incidents in one session, both disguised.** A `cc` linker failure on `pond-mcp-server`
that was the allowance, not the code; and the SIGILL earlier that was `target-cpu=native`. Section 3
covers both. The pattern: **on this container, a build failure is more likely to be the environment
than your change** -- check `df` and `RUSTFLAGS` before reading the diff.

### Next: the `SecurityPolicy` deny matrix (PAI-1 P4 / PAI-2 P1), then PAI-3 P3

`TokenCounter` port, GGUF-backed adapter, chars/4 as the declared fallback, overshoot-feedback
correction retained as the safety net. `turn_trimmer.rs:89-91` is the estimator to displace, and
`WindowSource::is_exact()` already exists to tell budget code when it can trust the window to the
token.

**2026-08-05 — the three open decisions are closed. Jerry's calls, recorded verbatim in intent.**

The previous three entries each ended by asking for a decision rather than taking one. All three are
now answered, so nothing downstream has to guess:

1. **Capturing a household member at pairing time gets its own phase.** Not folded into PAI-1 and not
   into PAI-7, both of which need it. Burying it in either makes the other's dependency invisible in
   the status ledger — and it is self-contained anyway: a migration, one question in the pairing
   flow, and a resolver input that already exists and is fed `None` by every caller. PAI-1's
   `paired_device_profile` and PAI-7's "that profile's devices" both become live when it lands.
2. **The unidentified-speaker posture stays implicit, and gets documented.** `Household` in a
   one-member pond, `Guest` once there are two — no setting. The argument in PAI-1 P3 holds: the two
   scopes only describe different rows when there is somebody to be excluded from, so in a one-member
   pond they are the same rows. It belongs in release notes, not in `Settings`. **Consequence for
   P4:** enforcement is being built on a posture the user never explicitly chose, so the audit-mode
   telemetry PAI-2 P1 collects is the thing that has to reveal it if the call was wrong.
3. **`approve_draft`'s missing ownership check belongs to PAI-2, with the deny matrix.** Not
   hoisted into P0 alongside the auth allowlist, and not landed as a standalone handler patch.
   It becomes a real production call site for `SecurityPolicy::allow`, which today returns `Ok(true)`
   with none — so the check and the layer meant to express it land together, in `audit` mode first
   per PAI-2's own plan. It stays a known live hole until then: any session can approve any draft id,
   and the guest tool-group gate does nothing about one member approving another's.

   **Done 2026-08-05, and the call was right for a reason I had not anticipated.** Landing it with
   the policy layer forced the question "who is the caller?", and the answer turned out not to exist
   yet: `drafts` had no owner column *and* its `session_id` was a model-supplied parameter
   defaulting to `"default"`. A standalone handler patch would have compared the approver's session
   against a field that is the same string for every draft on the pond, passed its tests, and shipped
   a check that could never fire.

**2026-08-05 — first macOS run of `live-test.sh`. It wrote to a real pond, and that is the finding.**

The script's own header promises "a scratch `POND_DATA_DIR` so it can never touch a real pond." That
promise did not hold, and the way it failed is worth more than the fix.

A `pond-server serve --native` had been running on this Mac for four days, with no `POND_DATA_DIR`,
holding port 4000. The script hardcoded `PORT=4000`, waited 60s for `.runtime_api_port`, and on
timeout **kept the assumed port and carried on**. Its own scratch server had meanwhile fallen back to
4001. So every HTTP assertion, and both onboarding lift writes, went to the real pond:
`PUT /settings {"user_name":"LiveTest","chat_model":"mock",...}` overwrote four real settings rows.
Restored from surviving evidence — `primary_profile_id`'s display name, `active_llm_model`, and the
model on the last 113 sessions — with a timestamped `.bak` of the database taken first.

**The 60s timeout was not arbitrary and was still wrong.** `.runtime_api_port` is written right after
`bind_with_fallback`, which on a cold macOS start lands *about* 60s in. The wait was sitting exactly
on the boundary. But the timeout length is the small half of the bug: the real defect is that
expiring was **not fatal**. Falling back to an assumed port is a widening default in the same family
invariant 2 names — on failure it reached *more*, not less.

**Three false readings this produced, all of which looked like real findings:**

- `no such table: _sqlx_migrations` — read as "migration 0037 did not apply". `sqlite3.connect`
  *creates* an empty database when the path does not exist, so a wrong data dir is indistinguishable
  from a failed migration. `db()` now refuses to invent one.
- Five `FAIL GET /... returned 000 with NO TOKEN` — read as a catastrophic auth hole. It was the
  auth-probe server not having finished starting; the health wait had no failure check and the route
  loop ran regardless. Exactly the class this suite's own docstring warns about: **a check that fails
  because the request failed reports the opposite of the truth.** Same trap, one layer out.
- A Playwright console-error failure — read as a UI defect. It was the browser driving the *real*
  server, which has no loopback bypass, so six calls 401'd. Once the port resolved correctly, all four
  live UI tests passed with **no change to the spec**. Worth recording as a near miss: the obvious
  move was to filter 401s out of that assertion, which would have masked the port bug permanently and
  left a correct test weaker.

The generalisable lesson, and it is the same one as `ProfileScope::Owner`: **ask what the check does
when its setup is wrong.** A harness that cannot tell "the thing I am testing is broken" from "I am
not testing the thing" reports the second as the first, confidently, while the parts that never ran
report nothing at all.

`live_checks.py` now resolves the port once and treats its absence as fatal setup rather than a
per-check failure — previously a `FileNotFoundError` escaped mid-`section_identity`, so P3 through P8
never ran while the run still reported on the sections that had.

**Fixed and re-run clean:** 37 API checks / 0 failed, restart OK, migrations applied once, 4/4 live UI
tests, one benign WARN in the log dig (`auto_download` skipping `llamafile/mock`, which is the live
check's own onboarding write). `rc=1` remains, from the auth section alone, exactly by design.

**2026-08-05 — PAI-2 P0 LANDED. `scripts/live-test.sh` is green end to end for the first time.**

`is_public_route` takes a `&Method`; the allowlist is a `(Method, &str)` table with segment-wise
`{brace}` matching. Details and the exception rationale are in PAI-2's P0 entry. Three points worth
carrying forward rather than repeating:

- **The router's public/protected split is about ONBOARDING; `is_public_route` is about AUTH.** They
  read like one axis and are two. That is why P0's own acceptance test — "every route in
  `protected_routes` requires a token" — could not be written as a blanket assertion:
  `GET /oauth/callback` and `POST /oauth/refresh` sit in the protected router and are legitimately
  reachable without a bearer token, each guarded by something else. Anyone writing that test from the
  design text alone would have concluded it had found two more leaks.
- **The guards were mutation-tested.** Re-adding `(Method::GET, "/settings")` to the table fails all
  three, naming the route. Given three recorded vacuous-test incidents in this programme, a guard is
  not evidence until it has been made to fail — and this one parses source text, which is exactly the
  kind of check that silently matches nothing. A `routes.len() > 50` assertion pins that too; the
  parser currently sees 100 protected registrations, and a sweep confirmed every one uses a verb it
  recognises (no `any`/`head`/`options` routers exist to slip past it).
- **The stale "FAILS BY DESIGN" epilogue is gone from `live-test.sh`.** It told the next person that
  an auth failure was expected. Leaving it would have trained them to ignore the one section most
  likely to catch a real regression.
- **A test was passing for the wrong reason, and only the fix could reveal it.**
  `settings_is_blocked_before_onboarding` asserted 403 with no token — which held only because
  `GET /settings` was public, so the request reached the onboarding guard instead of being refused
  at auth. Correct status, wrong gate. This is the fourth member of this programme's family of
  tests that assert something true for a reason that is not the one claimed, and the first found by
  a *fix* rather than by an audit. Add to 2.2: when a change makes a test fail, check whether the
  test was ever measuring what its name says before assuming the change is at fault.

Gates: `pond-api` 109 lib and 137 across all 17 integration targets (135 + 2), `pond-core` 736, fmt
and clippy clean, `cargo build -p pond-server` via the live run.

**One new WARN appeared in the log dig and is environmental, recorded so it is not rediscovered:**
`embedding provider failed to init: embedding provider init timed out after 30 s — ONNX Runtime may
be version-incompatible (need ORT 1.24.2)`. macOS-side ORT version, unrelated to this change and not
present on every run (it is a 30s timeout, so it is load-dependent). The other WARN,
`auto_download` skipping `llamafile/mock`, is the live check's own onboarding write and was already
recorded on 2026-08-04.

**2026-08-05 — PAI-1 COMPLETE. P4 landed; P5 was found inert and repaired.**

Two things I would want the next person to take from this rather than the code.

**A phase can be stamped LANDED and be inert.** P5's tool-group denial sat inside
`resolve_session_tool_groups`, reachable only from the `tool_selection_is_relevant()` branch, while
`default_tool_selection_mode()` returns `"all"`. Every test exercised the branch the feature lives
in, which is the natural thing to test and the reason nobody noticed that the default configuration
never enters it. For a day, a `Guest` on any default pond kept `giap-memory` and could read or
`forget_memory` the whole household. Add to 2.2: **verify a phase against the DEFAULT configuration,
not only against the code path it added.** "I tested the feature" and "the feature is reachable" are
different claims, and the second is the one that matters.

**The specified deny matrix would have been theatre, and building it would have felt like progress.**
Eight scopes x three `PrincipalKind`s is twenty-four cells and every one has to be `allow` — each
kind genuinely needs each scope, and denying `Internal` breaks background work silently. Keying on
`PrincipalKind` cannot express the thing that actually discriminates, which is whether a caller has
*proved* the identity it claims. So P4 landed one rule that means something —
`is_identity_assertion_proven` — against the live hole recon turned up: `PUT /sessions/{id}/user`
took a `profile_id` from the request body and bound it at `Explicit` strength with **no ownership
check at all**, after which every turn resolved to that member's scope. Any paired device could be
any member. Worth generalising: **when a specified design produces a uniform answer in every cell,
the axis is wrong, not the rules.**

`allowed` and `denied_reason` are deliberately separate on `PolicyDecision`. In `audit` a refusal
still proceeds, so recording only the effect would log "permitted" for exactly the requests
`enforce` would block, and the audit trail could not answer what flipping the mode would break.
`verdict()` reports `allow` / `would_deny` / `deny`. This is the same failure family as the three
false readings the live harness produced earlier today, and it is now four for this session.

**What PAI-1 completing does NOT mean.** `enforce` refuses every remote explicit identification,
because nothing can prove an identity — no schema links a paired device to a member, and that is its
own phase (Jerry's call, recorded above). The mode ships as `audit` for exactly that reason. Also
still open and unchanged: the `giap-memory` MCP tools pass `ProfileScope::Household` directly, so an
*Owner's* tool calls remain unscoped; the guest gate covers a visitor, not one member reading
another's through a tool call. Closing it still needs a per-turn cell in the MCP server.

Gates: `pond-core` 751, `pond-api` 109 lib + 39 in `agent_data_integration_test`, `pond-infra` 188,
`pond-adapters-goose` 103 lib with all test targets building again, fmt clean, and
`scripts/live-test.sh --ui` green end to end.

**2026-08-05 (later) — PAI-2 P4: the secret store is encrypted, and the interesting part was not the
cipher.**

`secrets.json` is now an XChaCha20-Poly1305 envelope under `<data_dir>/secrets/master.key`. The
crate was already in `Cargo.lock` and the cipher choice took ten minutes. What took the rest was the
same failure family as everything else in this programme: the old constructor parsed the file with
`serde_json::from_str(&content).unwrap_or_default()`, so a file it could not read became an **empty
store**, with no error and no log line, and the next `set()` wrote over it. On failure, access
widened. That is the P0 allowlist bug wearing different clothes, and it is now five for this
programme.

**A guarantee whose only evidence is a comment is not a guarantee.** The plan for this phase said
the migration ordering — key durable *before* ciphertext, the property standing between a power cut
and secrets nobody can ever open — was implemented and documented but not testable without fault
injection it was not adding. It is testable: force the ciphertext write to fail (pre-create
`secrets.json.tmp` as a *directory*, defeating `create_new(true)`) and assert the key is already on
disk. Reversing the order fails that test **and no other**, which is the whole argument for writing
it. When a plan says a property cannot be tested, the useful question is what would have to be true
for it to be observable, not whether the prose is convincing.

**A first start proves nothing about an on-disk format.** The process that encrypted the file is the
one reading it back, out of a cache it never dropped. `live-test.sh` restarted the server and then
asserted only over the database, so the store had no restart coverage at all; it now leaves a secret
behind on the first pass and reads it back on the second. Verified by swapping the key between the
two starts.

**Two operational facts that will bite somebody.** Downgrading below this commit destroys the store:
an older binary hits that `unwrap_or_default()`, reads the envelope as empty, and writes plaintext
over it. And `giap.sh doctor` now FAILs on every pond not yet restarted on a P4 binary, mine
included — those stores really are plaintext, and the check is right.

**Say what it protects, not what it sounds like.** Key and ciphertext share a directory by default,
so this defends a copied file, a backup, a support bundle — not a running pond, and not a lifted SD
card unless `POND_SECRET_KEY_FILE` puts the key elsewhere. The four `api_key_*` fields are still
plaintext rows in `settings` until P2 moves them, so "GIAP encrypts your API keys" is not yet true.

Gates: fmt clean, clippy and test green on the fast set (`pond-infra` 202 lib, up from 188), `cargo check` on
`pond-server` + `pond-adapters-goose`, and `scripts/live-test.sh --ui` green end to end including
the new restart pass.

**2026-08-05 — PAI-2 P3 (redaction). The reusable lesson is about where a guard lives.**

The rules were the easy half. The wiring was the half that was wrong, twice, in a plan whose author
had read the code carefully: `run_server` builds the memory repository once, but `main.rs` builds it
four times, and three of those are write paths reachable from `pond chat`, the voice loop and
`pond memories add`. `SqliteSecurityPolicy` is handed its own independently constructed
`SqliteEventLog`, so wrapping the shared binding covered every event except the audit trail. Both
mistakes compile, run, and look right in review. Grep for the CONSTRUCTOR across the whole file, not
for the binding you were told about.

**A guard belongs where CI runs it, not where the code is.** `ci.yml` has no `cargo test -p
pond-server`, so any wiring guard written next to `main.rs` would never fire on a pull request. The
guard for this phase reads `main.rs` with `include_str!` from `crates/pond-infra/tests/` — no
dependency edge, no link, and it runs in the fast pass. It found a fourth bypass on its first
execution. The same technique is available to P6 for the "an outbound body path has appeared and
nothing redacts it" guard that P3's own risk list said could not be written.

**Watch a guard fail before believing it.** Four mutations were run and all four produced messages
that name the defect: the audit sink unwrapped (`main.rs:2593 builds a write-path SqliteEventLog
outside RedactingEventLog`), `set_egress_sink` rebound to a fresh log, the UK inward-letter set
replaced with `is_ascii_alphabetic` (`mangled: Play B2 3AM by the band when I get home.`), and one
`MemoryRepository` forward deleted.

**A negative test is the product decision.** A redactor that eats ordinary prose is worse than none,
because the user stops trusting the transcript and turns it off. Every rule here has a real-shaped
false positive next to its positive, and it is the negatives that shaped the code: no dot in the
phone separators (IP addresses), a trunk prefix required on bare digit runs (epoch timestamps), Luhn
required on card-length runs (order numbers), the real inward-letter set on postcodes (`B2 3AM`),
and an unprefixed 64-hex secret deliberately left undetected because a git SHA is indistinguishable
from it.

**Live-run trap, already documented and still worth repeating.** `live-test.sh` dying at "never wrote
`.runtime_api_port` after 180s" is `ensure_onnx_runtime` downloading ~30 MB into a fresh scratch
directory, not a hang in your feature. Export `ORT_DYLIB_PATH` at an existing copy. The comment
above the start block says so; I lost a full run to not reading it.

**2026-08-05 (last of the day) — PAI-2 P7. The closure had to be reversible, and that decided the
design.**

The obvious implementation of "close the onboarding holes" is a flag: remember that this pond has
been set up, and stop answering the wizard's writes. It is wrong, and one route is what makes it
wrong. `POST /onboard/reset` is public and stays reachable after onboarding because it is the
recovery lever for a misconfigured pond — and it works by making the pond *not onboarded* again, so
the wizard's writes have to come back. A flag never comes back. Reset would have succeeded, dropped
the pond to the wizard, and left `PUT /settings`, `POST /profiles` and `POST /onboard/complete`
shut, with reflashing the device as the only repair. **Generalise: before closing a door, find the
route whose whole purpose is to reopen it.** The mirror of that error is leaving reset
unconditionally public, which makes the closure decorative — reset, then walk in. Both readings are
defects; neither is a trade-off. Reset is loopback-only once the pond is onboarded, which is not a
new boundary: `handshake_pairing_code` and `handshake_issue_pairing_code` already refuse a
non-loopback peer inside the handler, so a pond that has lost every token can only be re-paired from
the host anyway.

**I was told to use a defaulted trait method and I should not have been.** The plan gave
`OnboardingRepository::is_complete` a default body reading `get_current_step`. Eight implementors,
mostly test stubs — and a stub that inherits "not onboarded" makes every onboarding write route
public wherever it is used. That is a widening default, which is this programme's own most-repeated
bug, and PAI-1 invariant 2 exists to forbid it. The method is required instead. The compiler named
all eight in one pass and each one said what it meant. **A default on a trait that answers a
security question is a decision made by whoever forgot to override it.**

**A scope-widening default, found by asking what happens when the read fails.**
`SqlxOnboardingRepository::get_current_step` ends `.ok()??`: a database error becomes `None`, and
`None` means "not started" to every consumer — the exact state in which every onboarding write hole
is open. Keying auth on that answer would have reopened all of them on a set-up pond for as long as
SQLite returned `BUSY`. The port now has `is_complete() -> Result<bool>` and the gate treats a
failed read as onboarded. Add to 2.2: **when a new decision starts reading an existing value, read
what that value does on failure, not only what it means on success.**

**And a trap worth remembering for any port with an `Arc` blanket impl.** `AppState` holds
`Arc<dyn OnboardingRepository>`, so method resolution picks the impl on `Arc` before the concrete
adapter. A default plus a SQLite override would have compiled, read correctly, and never executed
the override — the default body would have run on the `Arc`, called `get_current_step`, and answered
"not onboarded". Requiring the method makes deleting the forwarding arm a compile error.

**A compile-time guard cannot ask a state-dependent question, so change the question, not the
guard.** The three `PUBLIC_ROUTES` drift guards parse `routes.rs` with `include_str!` and have no
pond to read. Picking a state would have reported the other state's answer as safety. They now ask
`reachable_without_token_in_some_state` — the worst case, the only thing a static check can answer
honestly — and a fourth guard pins the exact list of state-dependent entries so a route cannot
change class quietly. None was weakened to fit.

**The plan said close `POST /tts`; the shipped clients said otherwise.** It looks like an onboarding
hole — the wizard's voice preview is why it is public — but `playTtsSentence` (WebVoiceBackend.ts)
and `fetch_tts_bytes` (audio_cmd.rs) both call it with no `Authorization` header long after setup,
so closing it makes the assistant mute on a set-up pond. `POST /voice/calibrate` *is* closed,
because `calibrateWakeWord` does attach the token. The difference between the two decisions is that
I opened the client and read it. **Before narrowing a route, grep the clients that call it — a
classification derived from what the route is for is a guess about what calls it.**

**Third instance of a test asserting the right thing about the wrong fixture.**
`put_settings_without_a_token_is_still_allowed` ran against `OnboardingStep::Completed` and asserted
the write stays open. The assertion was correct for a pond mid-onboarding and described the hole for
a pond that was set up. P0 found the same family from the gate side
(`settings_is_blocked_before_onboarding`); this one came from the fixture side. Both times the test
name was true and the setup was the lie. The same fixture also had to stop using
`MockSettingsRepository`, which silently drops `chat_model` — the field `complete_onboarding`
refuses to proceed without — so a wizard round trip against it could never have finished.

**The mutation, and what it printed.** Replacing the live read with a process-wide `EVER_ONBOARDED`
latch failed `reset_then_recover_is_not_a_one_way_door` at step 3: `after a reset the wizard must be
able to save again, left: 401, right: 200`. That is the one-way door, named, in the message. A guard
for this class of defect is worth nothing until you have watched it print that.

Gates: fmt clean; `pond-core` 783 + 5, `pond-infra` 214 + 3 + 3, `pond-api` 112 lib + all 17
integration targets; clippy clean on the fast set; `cargo check -p pond-server
-p pond-adapters-goose`; `scripts/live-test.sh --ui` green, and its no-bypass auth section now
asserts the PAIR (`PUT /settings` 200 with no token before `POST /onboard/complete`, 401 after) plus
the whole reset-then-recover round trip over real HTTP. Asserting only the 401 would pass on a
server that never started.

**2026-08-06 — PAI-3 P3 PARTIAL: the catalog data landed, the rung is still dead.**

`WindowSource::CatalogRecord` — precedence rung 3 — **has never been producible in production.** All
three `ContextInputs` construction sites (`goose_agent.rs::resolve_window_with`, `routes.rs`'s
`turn_context_limit`, and the quarantined `pond-agent/src/agent.rs`) pass
`catalog_context_length: None`. The only thing that has ever produced that variant is the unit test
in `context_governor.rs`. This is the `ProfileScope::Owner` defect exactly: a branch with full test
coverage, reachable from no fixture production can build.

Two things it hid, both found by asking *who writes the field* rather than *is the field written*:

- The checklist's own 2026-08-03 correction — "`ModelRecord.context_length` *is* written for the
  curated GGUF catalog" — was true and stopped one question short. `gguf_record` writes it;
  `llamafile_record` and `ollama_entry_to_record` write `None`. **Ollama is the provider class the
  rung was designed for**, and it was the one with no data.
- Every Gemma 4 row declared `8192`, copy-pasted from the Gemma 2 rows directly above it in the same
  table. The real declared window is `131072`, verified against
  `model_info["gemma4.context_length"]` from a live `POST localhost:11434/api/show`. Nothing caught
  it in the seven commits since, because nothing read the field.

**A dormant column is not a safe place to put data.** It rots at the rate the catalog is edited and
nothing pushes back. That is the general lesson, and it is the same shape as the vacuous-test family:
the absence of a reader is the absence of a check.

Landed (all in files no other workstream holds): the llamafile table gained `context_length`; the
Gemma 4 rows were corrected; `OllamaCatalogProvider` now reads the window from `/api/show`, matching
the `model_info` key by `.context_length` suffix rather than an architecture allowlist that would go
silently dead on the next family; and rung 3 gained the clamp it was missing — a catalog value is a
DECLARED MAXIMUM, not an allocation, so for a local provider it is bounded by
`UNPINNED_LOCAL_CEILING`. Without that clamp, populating the field correctly would have been the
regression: 131,072 tokens of history budget on a Mac that allocated 32,768.

**Blocked, and deliberately not worked around.** Supplying the value at the two live call sites needs
`goose_agent.rs` and `routes.rs`; surfacing it in the Models UI needs `record_to_dto` (in
`routes.rs`) plus `ModelStatusEntry` and `types.ts`. All held by a concurrent session. Deferred to
**P3b** rather than edited carefully — PAI-2 lost an encrypted secret store to two phases writing one
file, and it compiled with every test green. Note for P3b: `Models.tsx`'s `inferCapabilities` derives
the context window from the model NAME in the frontend, a third copy of the heuristic this workstream
exists to delete; replacing it is the point of surfacing the field, not a bonus.

**The mutations, and what they printed.** Removing the local clamp from rung 3 failed
`a_catalog_length_cannot_widen_an_unpinned_local_window` with `left: 131072, right: 32768` — the
defect, in the numbers, in the message — with the other sixteen governor tests still green, so the
guard is doing the work and not a blast radius. Reverting `llamafile_record` to `context_length: None`
failed `every_chat_capable_entry_declares_a_context_window` with `catalog entry llama-1b declares no
usable context window (0); rung 3 of the context governor reads this field`, and took
`the_same_base_model_declares_the_same_window_in_both_tables` down with it. Both restored, both green
after.

**2026-08-06 — PAI-3 P6. A partial correction is how a document ends up contradicting itself.**

`token_tracking.md` and `model_capabilities.md` had both already been "corrected" in the 2026-08-03
documentation-debt pass, and both were still wrong in the same shape: the fix landed in one place
and not in its neighbours. `token_tracking.md`'s prose said real provider usage is the primary path
while the ASCII flow diagram three lines above it still had `chars / 4 heuristic` on the main arrow.
`model_capabilities.md` gained `tool_calling` in the struct listing while the detection table below
it kept five columns and the REST sample below that kept five keys — and since the handler
serialises the struct whole, the wire has carried six fields the entire time. **When a claim is
corrected in a document, grep the document for every other place that claim appears.** A partial
correction reads as authoritative and is harder to spot than the original error.

**Four claims were false, not merely incomplete**, and two were only findable by reading the
frontend:

- The `~` prefix does not mean "estimated". `formatTokens` in `UsageStatsCard.tsx` applies it to
  every count of 1000 or more as a rounding marker for the `k` suffix and prints smaller counts
  bare. It takes a number and nothing else; `UsageStats` carries no provenance to read.
- `trim_to_budget_for_model` was cited as what `context_window_tokens` drives. Its only call sites
  are its own unit tests, below the `#[cfg(test)]` line in the same file. A function with tests and
  no callers looks alive to any grep that stops at the first hit.
- `qwq` was documented at 32K. The context arm matches the substring `qwen`, which `qwq` does not
  contain, so it falls through to the 4,096 default. The table row grouped `qwen3 / qwq` together,
  which is what hid it — **a doc table that merges two patterns into one row cannot express them
  diverging.**
- `Models.tsx`'s `inferCapabilities` claims to mirror `from_model_name` and does not: no E1B
  exclusion, so `gemma-4-E1B-it` gets a Vision badge the backend deliberately refuses; no
  `gemma-3n` spellings; no `vl`/`vision` segment rule; and it computes neither `structured_output`
  nor `tool_calling`. Recorded, not fixed — it is badges only, but it is a duplicate of a rule whose
  entire design is an argument about which way it is allowed to be wrong.

**Line numbers, again.** `03-context-governor.md` cited `token_tracking.md:28`, and the roadmap's
debt table cited both files by line; the rewrites move every one. All three citations are now
symbols or section names. That is the second time PAI-3 has rotted a citation by editing the file it
points at.

**No gates were owed, and that is worth stating rather than leaving implicit**: the phase touches no
Rust, no `Settings` field, no migration, no route and no startup wiring, so `cargo` and
`live-test.sh` have nothing to say about it. They were run anyway, to prove that claim rather than
assert it. `cargo fmt --check` clean; `cargo test -p pond-core` 749 passed, **4 failed** — all four
in `context_budget.rs`, which is the continuous-curve work of a concurrent phase, mid-edit, in the
working tree. A documentation phase that reports a red suite it did not cause is more useful than
one that reports "green" by only running what it likes.

**The mutation, since a doc that asserts a guard owes evidence the guard bites.** Section 3 of
`token_tracking.md` now claims `GOOSE_CONTEXT_LIMIT` is written and never read, on the strength of
two `include_str!` source-scanning tests — exactly the class of check that can pass by matching
nothing. Putting `std::env::var("GOOSE_CONTEXT_LIMIT")` back into `ContextGovernor::prompt_window`
failed `this_module_does_not_read_the_environment` with `context_governor must not read process
environment variables`, with the other sixteen governor tests still green, so the guard is doing the
work and is not a blast radius. Restored; the file's md5 is byte-identical to before the mutation,
which matters because the file carries another phase's uncommitted work and `git checkout --` would
have destroyed it. Its twin in `goose_agent.rs` was **not** mutated — that file is held by a
concurrent session — and `token_tracking.md` says so rather than implying both were proven.

What the phase did owe beyond that was verification:
every claim was re-checked against `git show HEAD:<path>` rather than the working tree, because a
concurrent session is mid-edit in `goose_agent.rs`, `turn_trimmer.rs` and `routes.rs`, and
documenting somebody else's uncommitted work as current state is its own way of shipping a lie.

**What that discipline caught.** The recon for this phase reported the PAI-3 documents as clean;
they were not. PAI-3 P3 had landed its catalog work into the same three files in the meantime, so
the status rows this phase had to stamp were already carrying `P3 PARTIAL`. They were merged, not
overwritten. Both rewrites also had to stop describing rung 3 as "PAI-3 P3" future work: the durable
statement is that **no production caller supplies `catalog_context_length`**, which is true both
before and after P3's data work, and is the thing P3b changes.

---

**2026-08-06 — PAI-3 P5 code landed. The measurement that decides it did not.**

P5 is "asymmetric budgeting: preamble capped, working set scaled", and the first surprise was that
the cap already existed. P1 had moved it into `ContextGovernor::prompt_window` and two adapter sites
called it. The defect was that calling it was **optional**: `trim_goose_history` and
`hydrate_goose_session` built their profile straight from the raw window, so a single Orin turn
carried two different preamble budgets depending on which function you read — 3,600/700 on the
history side, 3,000/500 on the side that actually assembled the prompt. That is the four-paths-
disagree shape PAI-3 exists to remove, reproduced one layer down inside the fix for it.

So the asymmetry moved into the type. `CompactionProfile::for_windows(context, prompt)` takes both
windows, sources the preamble fields from the clamped one and everything else from the full one, and
**adds the difference to `history_token_budget`** — the preamble does not merely stay small, the
tokens it is denied go to history. Total budget is unchanged, which is what keeps P4's
"never promises more than the tiers did" property intact while the prefix/working-set split moves.
The profile gained one field, `prompt_window_tokens`, so `use_compact_prompt()` can be a preamble
decision rather than a context-window decision.

The second half is the clamp P4 deferred here **by name** in its own test comments: `turn_trimmer`
capped history at window-minus-reserve, which does not subtract the system prompt or the memory
block, so at 8,192 it let history claim 7,168 tokens on top of a 3,500-token preamble that was going
to be sent anyway. `usable_history_tokens()` subtracts it. `usable_prompt_tokens()` is deliberately
untouched, because the overshoot correction compares it against the engine's report of the **whole**
prompt and a history-only ceiling there would cry overshoot on every turn that merely spent its
budget. That distinction is the one thing in this phase most likely to be "tidied" wrongly later.

**What it costs, said out loud rather than buried.** At a symmetric 8,192 window effective history
drops from 4,000 to 3,668. That is less retained history at one operating point, and it is correct:
the 4,000 was never payable. Above the clamp history grows — 20,000 to 24,000 at 32,768 local, 7,200
to 8,000 at the Orin's pinned 16,384 — with the preamble frozen.

**Three mutations, because two of the three failure modes compile silently.** Ignoring the prompt
window: `a 4x window bought a bigger system prompt: 6000 vs 3000`. Reverting the trimmer clamp:
`history claimed 3810 tokens, past the 3668 the preamble leaves it`. Swapping the two `usize`
arguments at the adapter's `profile_for` — which the compiler cannot object to — `left: 8192,
right: 32768`. That third one is why the pure half was split out of `turn_profile`: `for_windows`
being right inside `pond-core` says nothing at all about the adapter feeding it correctly, and a
test that only exercised `pond-core` would have been the seventh vacuous test in this programme.

**Why the status cell says "code landed" and not "LANDED".** This document's own success criterion
for P5 is a measurement: `ttft_ms`, `prefill_ms`, `prompt_tokens` and retained turns, on Mac and on
Orin, before and after. There is no Jetson attached to the machine this landed on, and neither run
happened. The arithmetic says the local preamble is byte-for-byte what it was, because the same
clamp feeds the same two consumers — but "the budget did not change" and "TTFT did not change" are
different claims, and only the second one is what the phase promised. Stamping LANDED on unit tests
here would be exactly the substitution the PAI-2 P5 repair earlier in this log exists to warn about.

---

**2026-08-06 — PAI-4 P1. The design's tier table had two undefined cells and one overstated rule.**

`ModelClass { Small | Medium | Large }` and `CompactionStrategy` live in
`models/services/context/model_class.rs`, derived from the provider string plus whatever
`ContextGovernor::resolve` returned. Domain only, no call site — PAI-3 P1's shape, and P2 is the
first consumer. 15 tests; `pond-core` 733 lib, up from 718.

**The phase was specified as "derive `ModelClass` from the governor" and the work was almost entirely
deciding what the table in 3.1 actually means.** Each of its three rows names a window size *and* a
provider, which reads as one axis and is two. Two configurations that exist today fall between the
rows: an 8K hosted model, and a 131,072-token Ollama model on the Orin — the second of which PAI-3 P3
made reachable seven days ago by teaching `OllamaCatalogProvider` to read `context_length` from
`/api/show`, and rung 3 does not clamp it because Ollama is not a "local provider" by the governor's
definition. Implemented as two brackets plus a provider set, every row is reproduced and both cells
get an answer.

**The predicate that matters is not the one that was already there.** `ContextGovernor`'s
`is_local_provider` is `local | gguf`, and reusing it would have been the obvious move and wrong: it
asks whether the *preamble* is re-prefilled locally every turn. What the compaction tier needs to
know is whether an *extra* model call competes with the turn the user is waiting on, and on the Orin
Ollama and llamafile are HTTP to `127.0.0.1` off the same 102 GB/s. `ON_DEVICE_PROVIDERS` is
therefore four entries, not two, and it is a deny-list for the one permissive tier — safe when too
wide, unsafe when too narrow, which is why a provider that *could* point at another box is kept
inside it. Generalisable: **two predicates with the same name in English are not the same predicate,
and the cheap reuse is where a tier system quietly stops meaning anything.**

**I did not implement invariant 3 as written, and said so in three places rather than one.** The
small-tier row and the invariant both say no LLM in the on-device compaction path; the reason given
is a stall at 20 tok/s. The idle rolling summary cannot produce that stall — it runs after
`summary_idle_secs`, a new turn cancels it, and turns read `sessions.rolling_summary` without
awaiting it, which invariant 5 says in the same list. Following the wording would have removed the
rolling summary from the device whose window runs out first, to prevent a hang that path structurally
cannot cause. There *is* a real cost on-device and it is a different one — an idle summarisation
evicts the prefix cache, so the next turn pays a prefill it would not have — and it belongs to P5,
with a measurement, and it applies to the medium tier equally. The correction is written into 3.1,
into invariant 3, and into `strategy()`'s doc comment, because a claim corrected in one place and
left standing in its neighbours is how this programme has produced documents that contradict
themselves twice now.

**The consequence is that `Small` and `Medium` select the same strategy today**, which looks like the
classes are redundant. A test asserts the equality on purpose, naming the phase's own "today's
behaviour preserved" requirement, so that if it ever stops being true somebody has to mean it.

**`CompactionProfile` gained no field, and that was the load-bearing decision.** `turn_trimmer.rs`
builds it with exhaustive literals in two test helpers, so a new field is an `E0063` in a file this
phase does not own. The class is derived from `(provider, resolved_window)` — both of which every
budget call site already holds — rather than stored. Same property that let PAI-3 P4 land while
`turn_trimmer.rs` and `prompts.rs` were held.

**Two mutations, and the second one taught me something about the first.** Adding `ModelClass::Small`
to the `llm_resummarisation` arm failed `the_on_device_tiers_never_permit_an_llm_in_the_compaction_path`
with `the small tier selected a strategy that re-summarises with an LLM; on this tier that call
competes with the next turn's prefill (PAI-4 invariant 3)`, and took the behaviour-preservation test
with it — 13 of 15 still green, so the guard bites without being a blast radius. Dropping `ollama`
from `ON_DEVICE_PROVIDERS` failed three, including `ollama at 131072 tokens classified as large,
expected medium`. But it did **not** fail `an_on_device_provider_can_never_reach_the_large_tier`,
because that test iterates over the constant it is checking — it is a range property over whatever
the list happens to say, so shrinking the list makes it vacuously pass. That is the seventh member of
this programme's vacuous-test family and the first I caught by mutating rather than by review.
**A property test that quantifies over the data it is protecting protects nothing; the explicit table
next to it is what actually holds the line.** Both mutations restored, file md5 byte-identical.

Gates: `cargo fmt --check` clean, `cargo clippy -p pond-core --all-targets` with no new warnings,
`cargo test -p pond-core` 733 lib + 5 + 3 ignored, and `cargo check -p pond-server
-p pond-adapters-goose` clean. No live-server run: the phase touches no migration, no route, no
handler and no startup wiring, and adds no `Settings` field.

---

**2026-08-06 — PAI-4 P4. A gap is not a resume; a person is.**

`models/services/context/resume_compaction.rs` — `should_run(ResumeGateInputs) -> GateDecision`,
`idle_threshold_from_secs`, `idle_gap_since`, five skip reasons. 14 tests; `pond-core` 747 lib, up
from 733. New headless setting `resume_compaction_idle_secs`, full five-part ritual. Called from
`spawn_resume_compaction` in `routes.rs`, fired by `GET /api/v1/sessions/:id/messages`.

**The one-line rule in 3.2 hides the bug it would have caused.** It reads
`session resumed && idle_gap > resume_compaction_idle_secs -> compact`, and if `idle_gap` is the
only input, then at boot every stored session satisfies it — starting the server would compact the
entire history store and call each one a resume. That is consolidation's "never on startup" guard
wearing a different costume, and it needed a different implementation, not the same one: consolidation
asks "has a user done anything since I started?", which a background loop can answer. Here the gate
is not on a loop at all. `ResumeGateInputs::reopened` is set from a request somebody made, and
nothing in `pond-core` can set it from a timer. Generalisable: **when you port a guard, port the
question it answers, not the field it reads.**

**"Run full compaction on resume" could not be implemented as written, and the reason is that
compaction is two mechanisms with opposite timing.** The deterministic trimmer is a function of the
turn being assembled — there is nothing to pre-run and, being deterministic and model-free, nothing
to save. So the design's stated motive ("today compaction happens *during* the first turn back,
while the user waits on a token stream") does not describe what hybrid compaction actually costs a
turn. What it does describe, once you go looking, is a real hole one layer over: the idle
summary loop in `main.rs` skips any session whose `updated_at` predates process start, so a
conversation from before the last restart keeps a summary frozen at that restart, and the trimmer
splices the stale one into every turn until four fresh messages accumulate. That is what P4 fixes,
and I have said so in the phase stamp rather than claiming the larger thing. The large tier's
re-summarisation is P2's; this gate will call it when it exists.

**Both fallbacks lengthen, because short is the direction that costs.** 30 minutes for the default
(15x `summary_idle_secs`, 2x consolidation's inactivity threshold — the point at which the rest of
the system already considers the household asleep). `MIN_RESUME_IDLE_SECS = 300` floors a stored `0`,
which would otherwise mean "every reopen is a resume". `idle_gap_since` returns `Duration::ZERO` for
a future timestamp, so a skewed clock or a restored backup reads as *active*, never as stale. A
threshold that is too long only means the user pays what they already pay; one that is too short
spends a model call between every pair of turns on the device least able to afford it.

**Invariant 1 is held by where the code runs, not by a promise.** Both database reads the gate needs
happen inside the spawned task, so the reopen returns at exactly the speed it did before; the refresh
races a watcher on `last_user_activity` and persists nothing if cancelled. The in-flight set that
stops two rapid reopens queueing two model calls is process-local on purpose — a database row would
survive a `kill -9` and strand the session as permanently compacting.

**Three mutations.** Deleting the `reopened` check failed `a_huge_gap_alone_is_not_a_resume`
(`left: Run, right: Skip(NotAReopen)`) and `no_single_precondition_can_be_dropped` with
*"the gate ran with `reopened` unsatisfied"*. Dropping the `.max(MIN_RESUME_IDLE_SECS)` floor failed
`a_stored_zero_is_floored_rather_than_treated_as_no_threshold`: *"a stored 0 disabled the idle
threshold entirely — left: 0ns, right: 300s"*. Removing the `apply_key` arm failed pond-infra's
`roundtrip_persists_every_field` with *"resume_compaction_idle_secs: wrote 1807, read back
Some(Number(1800))"* — the settings ritual's own guard, checked because the field is the part of
this phase the compiler is least able to protect. A fourth, removing the `HEADLESS_BY_DESIGN` entry,
failed `every_settings_field_is_dispositioned` by name. All four restored; `git status --porcelain`
lists only the five files this phase owns.

**Learned, and not from the phase text.** `get_messages_paginated`'s doc comment says "returns the
100 most recent messages"; the SQL is `ORDER BY created_at ASC LIMIT ? OFFSET ?`, so the default page
is the *oldest* hundred. I had drafted the gap check against the last message of the page and it
would have measured the age of the conversation's opening line on any session over 100 messages —
i.e. it would have said "resume" forever. The gap is read from `sessions.updated_at` instead. The
doc comment is still wrong and is not mine to fix in this phase; it is recorded here so the next
person reads the SQL.

Gates: `cargo fmt --check` clean; `cargo clippy -p pond-core -p pond-infra -p pond-api
--all-targets` with no new warnings in the touched files; `cargo test -p pond-core` 747 lib + 5 + 3
ignored, `-p pond-infra` 214 + 3 + 4 + 1 ignored, `-p pond-api` 263 across 19 binaries; `cargo check
-p pond-server -p pond-adapters-goose` clean. No live-server run, and that is a gap rather than a
judgement: the phase adds a `Settings` field and a route side effect, both of which
`scripts/live-test.sh` exists to catch, and the integration assertion section 7 asks for — that the
refresh completes before the first token of the next turn — needs a real server and a real model.

---

**2026-08-06 — PAI-4 P6. A standing condition is not an event, and "reset after compaction" would
have undone the thing that makes it safe.**

`context_monitor.rs` — `claim_compaction`, `note_compacted`, `ContextState::turns_at_last_compaction`,
`COMPACTION_COOLDOWN_TURNS = 3`, and the health arithmetic lifted into a free `health_of`. Eight new
tests; `pond-core` 755 lib, up from 747. `routes.rs` — `spawn_pressure_compaction` fired from the
`should_compact` branch of `chat_stream`, `run_compaction_pass` shared with P4's
`spawn_resume_compaction`, and `reset_session` called from `delete_session`. One new wiring test,
`crates/pond-api/tests/context_monitor_reset_test.rs`.

**Where the code runs is what holds invariant 1, and the obvious placement breaks it.** The line the
phase is named after sits inside the SSE generator. The model has finished, but the `done` frame is
unsent and the client is still on the stream, so doing the work there puts a summarisation between
the user's last token and the end of their turn. `spawn_pressure_compaction` does nothing but
`tokio::spawn`; every read, the gate, and the model call happen in the detached task. Same shape as
P4, for the same reason, and it is worth stating as a rule: **"between turns" is a claim about the
stack you are on, not about the line number.** The `context_warning` frame is untouched.

**The design bullet contains no rate limit, and without one the phase is a regression.**
`should_compact` was safe to fire freely while it only decided whether to emit a frame. Driving a
model call makes it a rate, and utilisation is monotone — a session that crosses 75% is above 75% on
every later turn, so an unlimited rule summarises between every pair of turns on the device least
able to afford it. That is the identical failure P4 guarded against on the time axis with
`MIN_RESUME_IDLE_SECS`, arriving through a different door. `claim_compaction` grants one pass per
three recorded turns, recomputes health under its own lock so two turns finishing at once cannot both
be authorised by one snapshot, and stamps the cooldown on the **claim**: the model call is spent
whether or not the summariser finds anything to do.

**"Call `reset_session` on clear and after compaction" is two situations wearing one call, and the
second one is wrong.** A clear means the session stopped existing — drop everything. That is
`delete_session`, and it is the *only* such route: `DELETE /sessions/:id/user` releases an identity
binding and changes nothing about the context, so it is deliberately not a call site, which is the
narrower reading and therefore the right one. After a compaction the session is still here and its
window did **not** shrink: the rolling-summary refresh changes what the trimmer splices, not what the
engine reports next turn. Resetting there would zero utilisation, empty the growth window and clear
the cooldown stamp — so the pass would re-fire on the very next turn, the exact failure the cooldown
exists to prevent, reintroduced by the line meant to tidy up after it. `note_compacted` drops only
the growth samples (which genuinely measured a differently-shaped history) and runs only for
`RefreshOutcome::Refreshed`; `NothingToDo` and `Cancelled` changed nothing.

**Two things found while checking rather than assumed.** `reset_session` had no production caller at
all, so the growth map was also an unbounded leak — every session the process ever streamed a turn
for stayed in it for the life of the process. And section 3.4 cited "invariant 2" for the
between-turns rule; invariant 2 is the tool-result rule, and the one meant is invariant 1. Both fixed
in the same change.

**Mutations, both restored, `git status --porcelain` re-read after each.** Deleting the cooldown
comparison from `claim_compaction` failed three tests:
`a_saturated_session_claims_once_and_then_waits_out_the_cooldown` with *"claimed again only 1 turn(s)
into a 3-turn cooldown - a still-saturated session would summarise between every pair of turns"*,
`repeated_claims_within_one_turn_yield_exactly_one_pass` with *"5 concurrent claims were granted for
one turn — left: 5, right: 1"*, and `note_compacted_clears_growth_history_but_not_the_cooldown` with
*"note_compacted cleared the cooldown, so the next turn re-fired"*. Deleting the
`state.context_monitor.reset_session(&session_id)` line from `delete_session` failed
`deleting_a_session_clears_its_context_monitor_state` with *"the deleted session's utilisation
survived the delete — left: 85.44922, right: 0.0"*, which is the point of that test: it is a wiring
claim no unit test can reach.

**A restore that went wrong, recorded because it nearly cost the phase.** I reverted the first
mutation with `git checkout -- <file>`, which does not undo a mutation — it reverts the file to
`HEAD`, and took every edit of the phase with it. Nothing outside my own footprint was touched and
the work was re-applied, but the correct tool for an experiment on a file with uncommitted work is a
copy and a copy back. The second mutation was done that way.

Gates: `cargo fmt --check` clean; `cargo clippy -p pond-core -p pond-api --all-targets` with no new
warnings in the touched files; `cargo test -p pond-core` 755 lib + 5 + 3 ignored, `-p pond-api` 264
across 20 binaries; `cargo check -p pond-server -p pond-adapters-goose` clean. No live-server run,
and that is a gap rather than a judgement: this adds a route side effect that spawns a background
model call, which is what `scripts/live-test.sh` exists to catch, and section 7's integration
assertion — that a pass completes before the next turn's first token — needs a real server.

---

**2026-08-06 — PAI-4 P5. The cold half of the rule; P3 had already built the warm half without
being able to name it. Code landed, measurement not taken.**

`models/services/context/prefix_cache.rs` — `InvalidationReason` (the six designed reasons),
`PrefixCacheState { built_at, hash, turns_served, invalidated_by }` with
`invalidate`/`rebuilt`/`serve_turn`/`age_since`/`posture`/`posture_of`, and `CachePosture
{ Warm | Cold }`. Nine tests, no clock read in the file. `models/ports/agent.rs` —
`Agent::prefix_cache_state() -> Option<PrefixCacheState>`, default `None`, one test.
`turn_trimmer.rs` — an eighth argument and seven tests. `goose_agent.rs` — the state, the six
recording points, the port impl, and `provider_change_reason` with three tests. `pond-core` 802 lib,
up from 786; `pond-adapters-goose` 98, up from 95.

**This is stamped CODE LANDED, MEASUREMENT PENDING, not LANDED, and the distinction is the point.**
Section 7 of the design says P5 "is only correct if cold-cache recompaction shows no TTFT penalty
and warm-cache turns show no new re-prefills" — an Orin-and-Mac measurement, and its own acceptance
criterion. It has not been taken. That makes two measurement-pending P5s on the ledger alongside
PAI-3's, which is worth saying out loud rather than quietly accruing: a green unit suite proves the
rule fires where it was told to, not that firing there was cheap.

**Mac half taken 2026-09-24 (release binary, isolated pond; `docs/developer/realtime-inference-audit-2026-09-24.md`): warm-cache turns DID show new re-prefills, and the cause was neither compaction nor the lane.** Goose's `<turn-context>` block was prepended to a user message the engine had already cached, so every provider call re-decoded from that message's first byte: 471 tokens on the completeness-check inference, 589 on the next turn's first inference, and the entire conversation (10-21K tokens per turn) on a thinking-only session whose user messages goose had merged -- the shape the household pond logged all day. Fork patch `36413f065` appends the block instead; re-measured: 97 and 147 tokens, completeness-check TTFT 1.05 s -> 0.3 s (E2B) and 2.0 s -> 0.75 s (E4B). The summary-refresh lane job ran as `SacrificialContext` and evicted nothing. Still open on this axis: goose's completeness-check and tool-pair machinery rewrites history mid-turn (prompt 2,934 -> 2,618 with reuse falling to 1,340 on E4B), and the Orin half of the measurement is still owed. P5 stays MEASUREMENT PENDING on the device; on the Mac the re-prefills were real, attributed, and removed at their source.

**The design reads as two rules and only one of them was new.** 3.3 says recompact when cold,
prefer byte-identical edits when warm. The warm half landed on 2026-08-06 as P3, whose age rung
fires only when the conversation is over budget and cites invariant 4 for it — P3 simply could not
tell "warm" from "already gone", so it used *over budget* as a proxy for both. P5 supplies the real
signal and spends it in one direction only: the rung also fires when the cache is provably cold on a
conversation that still fits. Nothing on the warm path was tightened, because gating the summary
splice or the `<system-context>` strip needs the number section 7 is waiting for.

**The ordering bug I wrote first.** The obvious `serve_turn` clears `invalidated_by`. It is wrong
exactly on the case 3.2 calls the most valuable cold moment there is: `SessionResumed` is recorded
while the engine session is hydrated, hundreds of lines before the static prefix is compared *in the
same turn*, so an eager clear had a resumed session clearing its own resume and reading `Warm`. The
clear is deferred by one turn instead. The residue is a reason sticky for one extra turn, which
over-reports `Cold` once; that is a bounded over-count of a permission to degrade already-aged
material, where the opposite error silently forfeits the free recompaction the phase exists to take.

**A latent defect in P3's rung that only became routine under P5.** `truncate_head_tail` is not a
fixed point of itself — it keeps `cap` characters and appends an elision marker, so its output
always exceeds the cap and handed its own output it cuts again. P3 never saw it because its rung
only ran when over budget and one cut usually brought it under. Running the same rung on cold turns
that comfortably fit would have ground one tool result away by degrees over a long cold session.
`AGED_FIXED_POINT_CHARS` is the guard and `the_aged_cap_is_a_fixed_point_after_one_cut` fails if the
marker outgrows its allowance, rather than letting the property rot. I found this because I wrote
the idempotence test before believing the rule worked; it failed on the second pass, which is the
argument for running the property on every new trigger and not only on the one that introduced it.

**Two mutations, both restored by file copy rather than `git checkout`, per the P6 lesson above.**
Flipping the step-4 guard to `cache == CachePosture::Warm` failed
`a_cold_prefix_recompacts_a_conversation_that_still_fits_and_a_warm_one_does_not` with *"a warm
prefix was perturbed for a token saving nothing had asked for — left: 1, right: 0"*, and also failed
the pre-existing P3 test `age_weighting_never_touches_a_conversation_that_already_fits`, which is the
useful part: it proves the warm path really is pre-P5 behaviour and not merely asserted to be.
Deleting the `AGED_FIXED_POINT_CHARS` line failed `a_cold_recompaction_is_idempotent` with *"left: 1,
right: 0"* on the second pass. Both restored with no residue; full suite green after each.

**Two test failures found and NOT fixed here, because they are not this phase's.**
`goose_agent::tests::the_adapter_reads_the_catalog_it_was_given` and
`a_catalog_window_reaches_the_governor_from_the_adapter` both fail at HEAD (`fc78ec03`), asserting
`131072` where the governor now yields `32768`. That is the same shape f770f4de already corrected
twice: tests encoding the pre-clamp behaviour for an on-device provider. These two survived because
`pond-adapters-goose` is outside the fast-crate test set and CI only `cargo check`s it, so nothing
runs them. I verified they fail with my changes stashed before concluding they were not mine.

> **Corrected 2026-08-06, and "pre-existing" was the wrong word.** The diagnosis above is right in
> every particular and the stash check was the correct way to reach it — but the label reads as
> "somebody else's, from before this work", and these were **broken by `f770f4de` earlier the same
> session**, which is the clamp fix two commits upstream. Not the phase's fault; still this
> session's, and worth attributing rather than filing under inherited debt.
>
> The reason they were not caught at the source is mine and is the general lesson: `f770f4de` ran
> `cargo check -p pond-adapters-goose` and not `cargo test`. **A `check` proves a crate compiles and
> says nothing about whether its assertions still hold**, and for the one crate CI never tests, that
> gap is the whole safety net. Anything touching `context_governor` precedence must run
> `cargo test -p pond-adapters-goose`, because the adapter keeps its own copies of those
> expectations.
>
> Both are fixed now: the on-device case asserts the ceiling, and a hosted `openai` fixture was added
> beside it so the clamp reads as a boundary rather than a blanket. That is the **fourth** test found
> encoding the pre-clamp belief — two in `f770f4de`, two here — which is what a value duplicated
> across a domain crate and its adapter costs when it changes.

Gates: `cargo fmt --check` clean; `cargo clippy -p pond-core --all-targets` with no new warnings in
the touched files (the two the trimmer reports — a `nonminimal_bool` on P3's age predicate and a
`duplicate_macro_attributes` on an untouched test — are both pre-existing); `cargo test -p pond-core`
802 lib passing; `cargo test -p pond-adapters-goose` 98 passing with the two pre-existing failures
above; `cargo check -p pond-server -p pond-adapters-goose` clean. No live-server run and no device
run — the second is the phase's own acceptance criterion and is why this is not stamped LANDED.

---

**2026-08-06 — PAI-4 P7a. The manual button is the one place a rate limiter is easiest to argue
away, and the design bullet was wrong about the UI it named.**

The bullet is `POST /sessions/{id}/compact` plus a desktop control on the existing `ContextCard`.
The API half landed; the UI half is respecified and outstanding, and finding out why is most of what
I did before writing any code.

**`ContextCard.tsx` is not what the bullet thinks it is.** It is the MCP-UI tool-result card
renderer — it takes a `ContextCard` off `state/reducer.ts` and dispatches to the weather, devices,
memory and schedules renderers. "Context" there is tool-call context, not context window. A grep of
all of `pond-desktop/src` for `context_warning`, `contextWarning`, `should_compact`, `context_health`
and `percent_used` returns nothing, and `context_warning` is not even in the `ChatEventType` union:
the frame the server has emitted since before PAI-4 has zero consumers in the shipped app. So P7b is
a new surface across both chat views, not a button on an existing card, and it is scoped as such
rather than smuggled into this landing. AGENTS.md's "know what has no UI before writing a UI test for
it" is exactly this case — a Playwright test driving the assumed control would have exercised nothing
and passed, which is how this programme gets a seventh vacuous test.

**The endpoint does not bypass P6's rate limiter, and refusing to let it is the phase.** "The user
asked, so just do it" is the natural shape for a manual control and it is a regression here.
`should_compact` is monotone above 75%, so a press-driven pass is a *rate*: a button that skipped
`claim_compaction` would let anything holding a bearer token queue summarisation model calls in front
of the user's next turn on a serial on-device engine. The endpoint is therefore strictly a subset of
what the pressure axis already does — it can bring a pass forward within the rules, never past them.
The cost is real and named in the doc rather than hidden: a session below the threshold, and any
session the monitor has not recorded a turn for, answers `not_under_pressure`. Making the button work
under no pressure needs a wall-clock cooldown, because the existing one counts *recorded turns* and
with no turns happening it never expires. That is a phase, not a line.

**Two orderings deliberately differ from `spawn_pressure_compaction`.** It claims before reading the
provider, because the claim is the cheap check. This reads the provider first and peeks at
`COMPACTIONS_IN_FLIGHT` before claiming, because a claim spent on a pass that cannot run burns a
cooldown counted in turns — and a person pressing a button is very often taking no turns, so the
control would stay dead until they did. Two of the five tests assert that a refusal leaves the next
real claim available.

**The first draft of the guard would have passed against the bug it guards.** I wrote
`a_second_press_is_refused_by_the_cooldown` asserting `status == "skipped"`. Mutating the claim away
still produced "skipped", because a second pass finds the through-pointer already advanced and
answers `NothingToDo` — which is also a skip. The assertion that actually holds is `outcome is null`,
null exactly when no pass ran. With the claim disabled it fails with *"the second press ran a second
pass - the manual endpoint is bypassing P6's rate limiter, and a client hammering it would stack
summarisation model calls in front of the user's next turn on a serial on-device engine"* and the
body showing `"outcome":"nothing_to_do"`. Restored by reversing the edit; `grep "if false"` over
`routes.rs` returns nothing and the five tests are green again.

**A mock that was silently dropping the switch it was being asked about.** The first switch test
failed with `status: "compacted"` while `hybrid_compaction_enabled` was false:
`MockSettingsRepository` overlays a hand-picked subset of keys and neither compaction switch was in
it, so `update` wrote nothing and `get` returned the `true` default. Both switches now round-trip,
for the reason the mock's own `mic_enabled` comment already gives.

Gates: `cargo fmt --check` clean; `cargo test -p pond-api` all suites green including the 5 new
(115 lib + integration suites); `cargo test -p pond-core` 802 lib passing; `cargo clippy -p pond-api
-p pond-core --all-targets` with no new warnings from the touched files; `cargo check -p pond-server
-p pond-adapters-goose` clean. No live-server run — this adds a route, which is precisely what
`scripts/live-test.sh` exists to catch and no in-process `oneshot` router test can — and the endpoint
has never run against a real on-device summariser, only `MockProvider`.

---

**2026-08-06 (batch synthesis) — four phases landed in parallel, and the adversarial review found
more than the phases did. Read the HIGH dispositions before starting anything.**

Four implementation groups ran concurrently in disjoint footprints: PAI-4 P7b, PAI-2 P6a, PAI-5 P1
and PAI-5 P2. All four committed. This entry is the reconciliation, written after re-running the
gates rather than from the self-reports, and section 1's table plus the roadmap's table were both
updated to match — the three places had drifted before and `4b448ffe` is what repairing that costs.

### What actually landed

| Phase | Commit | Landed as | Not done |
|---|---|---|---|
| PAI-4 P7b | `d06341c0` | The `context_warning` frame's first consumer, in the hub chat | `sections/Chat.tsx` (was held); Canvas deferred; **the button cannot succeed — see below** |
| PAI-2 P6a | `55e68e9e` | 5 of P5's 6 ungated senders + the ORT `curl` subprocess; `MAX_UNGATED` 6 → 1; the mode installed before the first fetch on every entry point | `routes.rs` (P6b); `run_agent_cmd`'s install |
| PAI-5 P1 | `bbb11172` | The reasoning channel, gated once at the producer | `ChatEvent::Reasoning` + `pond-inference` (quarantined, deliberately); ThoughtFilter demotion |
| PAI-5 P2 | `ce8158b6` | `reasoning_tokens` producer + store, migration 0039 | The `turn_stats` SSE frame and `/usage/summary` (`routes.rs` was held) |

**Verified, not taken on trust.** `cargo fmt --check` clean; the full CI fast-crate set green (55
test binaries, 0 failures); `cargo test -p pond-adapters-goose` 109 passing; `cargo test -p
pond-server --lib --bins` 30 + 81; `cargo check -p pond-server -p pond-adapters-goose` clean;
`npm test` in `pond-desktop` 261/261. Two self-reported pre-existing build breaks were reproduced
and are real — see verification debt.

### Respecifications worth keeping

- **P1 gated at the PRODUCER, not the SSE seam, and that was the right call.** Recon called seam
  gating mandatory. There are THREE consumers of `AgentStreamEvent::Thinking`, not two — `routes.rs`
  twice and `pond-server/src/main.rs`'s CLI printer, which is the terminal voice loop writing to
  stderr unconditionally. Exactly one ever consulted `show_thinking`. A seam gate needed edits in
  two crates and would still have leaked reasoning to the speaker on `pond chat`, which is the
  precise thing PAI-5 invariant 2 forbids. One producer gate is inherited by all three. As a bonus
  the held-file collision on `routes.rs` evaporated instead of becoming a blocker. **Generalise
  this:** when a gate is being placed, count the consumers first; "two" is usually a grep of the
  crate you are already in.
- **P1's diagnosis in the doc was wrong, not merely stale.** Sections 1.2/1.3 said the reasoning
  channel was discarded in `pond-inference/src/provider.rs` — a crate that feeds the QUARANTINED
  PondAgent loop and has never executed in a shipped configuration. The live discard was
  `GooseAdapter`'s `as_concat_text()`. Following the doc literally would have produced a third
  correct-but-unreachable mechanism.
- **P6a added a startup-wiring component recon did not plan.** Without it the new ORT gate was
  provably unreachable on all three entry points, because `set_network_mode` had one call site.
  This is the recurring shape: a correct mechanism whose precondition nobody installed.

### HIGH findings from the adversarial review — what I fixed and what I did not

Fixed in this commit, each mutation-tested:

1. **P6a's file-level guard was satisfied by COMMENT PROSE.** `TRACKER_SYMBOLS` held bare symbols,
   so every real gate could be deleted from a tracked file with all six tests green, provided one
   of the phase's own comments mentioned the token. FIVE of ten tracked files were vulnerable; TWO
   were made vulnerable by comments P6a itself added. This is byte-for-byte the defect P6a found in
   its own ORDER guard and fixed only there. Now: call forms with the opening paren, plus a
   string-literal-aware `strip_line_comments` pass. Mutation — de-gating `vision_encoder.rs`
   keeping the comment — went from green to *"these files are listed EGRESS_TRACKED but CALL none
   of […] in code (comments do not count)"*.
2. **There was a FOURTH downloading entry point and the guard's detector locked it out.**
   `run_models` (`pond models download`) calls `model_download::download_file` twice and installed
   no mode, so P6a's gate there was inert and `offline` permitted a full model download — a privacy
   control failing OPEN. The detector asked "which functions call `ensure_onnx_runtime()`" and an
   `assert_eq!(callers, 3)` pinned that answer, so no other route could ever appear in it. Fixed at
   both ends: `run_models` installs the mode after `Database::init`, and the detector now asks
   which functions DOWNLOAD, count 4, with the two helpers in an explicit named exemption.
   Mutation — deleting the install, keeping the comment — fails naming `run_models`.
3. **The ORT gate, P6a's headline discovery, had no test at all.** The guard asserted only ORDER
   while its failure message talked about "the gate inside it". Deleting the `check_egress` line
   left all six tests green (`egress_tracked_files_reach_the_tracker` is satisfied by an unrelated
   OAuth `egress::begin(` elsewhere in `main.rs`). It is the only subprocess sender in the tree, so
   nothing `reqwest`-shaped will ever see it. Now asserted: `check_egress(` before
   `Command::new("curl")`, byte offsets, comments stripped. Mutation goes red.
4. **P1's gate was tested; its INPUT was not.** Changing `voice_instance || request.voice_mode` to
   `voice_instance` left all 108 tests passing. On the shipped desktop the instance flag is
   hardcoded false, so the request flag is the entire defence and that mutation leaks reasoning to
   every voice turn, silently. The source guard pinned the identifier `is_voice`, never its
   meaning. Fixed by extracting `GooseAdapter::voice_turn(instance, request)` and asserting all
   four rows, `(false, true)` first and by name.
5. **P2's "count is outside the display gate" guard was vacuous against its own claim.** The
   phase's mutation moved the statement inside the `for` loop, which the six-line window catches.
   Wrapping it in `if emit_reasoning { … }` one line up — the natural form — passed all seven
   reasoning tests. The harm is worse than a missed regression: `chat.rs` builds its `AgentRequest`
   with `voice_mode: true` unconditionally and is the only path that persists the number, so a
   gated count writes `Some(0)` for 100% of the corpus P5 reads. Now the whole eight-line window is
   scanned and the failure quotes the offending line.
6. **P2's carry-out was dark.** Replacing BOTH `UsageStats` arms with a literal
   `reasoning_tokens: None` passed all 108 tests — the phase's own named failure mode landing
   silently. Now asserted at 2 occurrences.

**NOT fixed, deliberately, with reasons:**

- **The "Compact now" button cannot succeed on any default configuration (P7b).** P6's pressure
  axis takes the shared `claim_compaction` quota one statement after emitting the frame, and the
  note only renders after `done` — so the claim is gone before the button exists. Six consecutive
  pressured turns produced six `cooling_down` refusals in a reviewer's probe. I verified the
  ordering in `routes.rs` myself. **This is a design decision, not a repair:** either the manual
  axis gets its own authorisation (keeping `COMPACTIONS_IN_FLIGHT`, which is what actually protects
  the serial engine) or the control stops rendering when the auto pass has claimed. P7a's stamp
  argues hard for *not* bypassing the limiter, and its guard
  `a_second_press_is_refused_by_the_cooldown` asserts the very thing that makes the button dead, so
  it must be re-derived either way. A synthesis pass should not silently pick a side. **This is the
  single most important item for the next round.**
- **P7b's guard is a two-substring grep and catches only a textual revert.** Two semantic mutations
  that leave the strings in place passed 261/261, including adding `showTurnStats` to the render
  guard — which would ship the note invisible on every default install, the exact mistake the stamp
  says it avoided by rejecting `TurnStatsFooter`. Fixing it needs a render test mounting
  `ChatHubView`, which needs the `AppContext`/api surface stood up. That is a frontend phase and it
  should land with the `sections/Chat.tsx` half, not bolted on here.
- **P1 emits one reasoning frame per DELTA, not per block.** On local/gguf — its headline reachable
  config — goose emits one `AgentEvent::Message` per token piece, so one reasoning passage renders
  as hundreds of one-fragment paragraphs with whitespace destroyed by the per-fragment `.trim()`.
  Confirmed by reading the submodule. This is a streaming-semantics change on the live path and it
  is exactly the kind of question one turn on a real Jetson answers better than a unit test, so it
  waits for the live run. The fix belongs in the adapter and needs a SEQUENCE test.
- **`ChatService::persist_assistant_response`'s reasoning link is untested.** Replacing it with
  `None` passes all 804 `pond-core` tests. One test against a fake `SessionStorage` closes it; left
  for PAI-5 P6, which is already opening that file.

### Deferred, with the reason

- **PAI-2 P8a** — needs `routes.rs` for the `SecurityPolicy::audit` signature change and to register
  `GET /security/policy-report` in `protected_routes` so it inherits P0/P7's four compile-time auth
  guards. Held during the batch. **The hold has ended**; the design in the doc applies unchanged.
- **PAI-5 P6** — a four-seam hold, all four now free: `settings.rs` (`persist_thinking`, all five
  pieces), `routes.rs` (accumulate `Thinking` into the persist call on both handlers, and rehydrate
  in `get_session_messages`), and `sections/Chat.tsx` (refill `thinkingBlocks` on session load; the
  render already exists). No unknowns left in it.
- **PAI-2 P6b** and **PAI-4 P7b's `sections/Chat.tsx` half** — same story, same unblocking.

Every one of these was blocked by concurrency, not by design. **The lesson for the next batch is
routing, not engineering:** `crates/pond-api/src/routes.rs` blocked three phases across two
workstreams in one run. It is the busiest file in the programme and scheduling two groups against
it costs more than running them serially would have.

### Verification debt — do not let this quietly become LANDED

- **PAI-3 P5 and PAI-4 P5** are code-landed with their deciding measurement outstanding. Both need
  warm-versus-cold TTFT on an Orin that is not attached to this machine. Their own design sections
  make the measurement the acceptance criterion. A green unit suite proves the rule fires where it
  was told to, not that firing there was cheap. Two measurement-pending P5s is now a standing item,
  not an aside.
- **`cargo test -p pond-server` does not compile**, and has not since `ProfileScope` landed.
  `crates/pond-server/tests/live_feature_test.rs` calls `MemoryExtractionService::run` without its
  `&ProfileScope` argument (E0061, 2 errors). Reproduced. CI has no `cargo test -p pond-server`, so
  it is invisible there and will stay invisible.
- **`cargo test -p pond-agent` is impossible** for the same class of reason: `agent.rs`'s `mod
  tests` builds `AgentRequest` without `profile_scope` / `profile_context` (E0063, 2 errors). CI
  runs `cargo check -p pond-agent` WITHOUT `--tests`, so it is green and will stay green. Fixing it
  means choosing a `ProfileScope` in a test fixture, which is a PAI-1 decision with an
  access-widening failure mode — take the narrowest scope, and do it deliberately.

Both of those are the same defect as the one AGENTS.md already warns about for
`pond-adapters-goose`: **a `check` is not a `test`, and for the crates CI only checks, nothing else
will ever run their assertions.**

### Needs the live run (`scripts/live-test.sh --ui`, coordinator, once, after this)

- **PAI-2 P6a — startup ordering changed on `serve`, `chat` and `setup`, and now `models` too.**
  This is the highest-value live check in the batch: `set_network_mode` moved, and a mode installed
  too late is indistinguishable from one installed correctly until something tries to download.
- **PAI-5 P1** — confirm reasoning frames reach the browser, render once and not twice, and
  **watch for the delta-granularity defect above**: if the thinking panel shows a wall of
  one-word paragraphs, that is the open finding reproducing, not a new bug.
- **PAI-5 P2** — migration 0039 must be applied against an already-populated pond. `live-test.sh`
  step 4 (restart against the same directory) is the case that matters; a migration that only works
  on an empty database works exactly once.
- **PAI-4 P7b** — a session crossing 75% should show the note in the hub chat. Expect the "Compact
  now" control to answer `cooling_down`; per the finding above that is the current behaviour, so a
  refusal is a CONFIRMATION of the defect, not a failure of the live run.

**2026-08-13 — personal-context blocker 0a: GGUF embedder landed, Mac-verified, Orin owed.**

Not a PAI phase of its own; it belongs to the [personal-context index](../personal-context-index.md),
which spans PAI-3/4/8. Recorded here because it fixes a live defect the checklist's own
verification story exists to catch: `embedding_provider = "fastembed"` shipped as the default and its
ONNX Runtime does not initialise on the Orin, so "semantic memory injection" (recorded landed under
PAI-3 Phase A) fell back to keyword on the hardware GIAP ships to — green everywhere, inert on the
device. A GGUF `EmbeddingProvider` over the llama.cpp this pond already runs replaces it
(`pond_inference::embedding`, selected by `embedding_provider = "gguf"`).

Run against 2.2: it puts **no** secret on `Settings` (the model is a file path, not a key); the
one-time model download is egress-gated (`HttpModelDownloader` → `egress::begin`); it does **not**
move the KV prefix or block a turn on an LLM call (embedding is a separate CPU forward pass, default
`n_gpu_layers = 0`, off the chat model's GPU); it adds no preamble tokens (retrieval is a tool/query
surface, not a prompt block). The one interdependency it turned up is new and sharp: **Goose's
local-inference `unreachable!`s on an already-initialised llama backend**, so a co-resident embedder
that wins the init race PANICS the live path. Handled by lazy model load (Goose claims the backend
first, on the first turn; the embedder wraps), which is why backfill must stay idle-gated.

**Verified on the Mac, NOT the Orin** — the device was offline. Per the vocabulary, this is
`LANDED`, not `VERIFIED`.

**Two things the Mac run found that unit tests could not, and both were mine.** First, the
coexistence mitigation I documented was WRONG, and the panic is reproducible without a Jetson: a pond
on `chat_provider=ollama` never initialises Goose's llama backend, the startup memory backfill embeds
immediately (so "loads lazily, after a chat turn" was false twice), the embedder wins
`LlamaBackend::init()`, and switching to a local model then panics a tokio worker inside Goose at
`llamacpp/mod.rs:355` — after which the API stops answering. Ordering could never have fixed it: any
embed claims the backend.

**FIXED the same day, and the patch set stays at 6.** That `AtomicBool` is llama-cpp-2's Rust-side
bookkeeping, not llama.cpp's — the C `llama_backend_init()` is idempotent (this crate already relied
on it) and `LlamaBackend` is a public field-less struct, so the token can be constructed safely.
`get_or_init_backend` now initialises the C backend directly and **never enters the CAS**, so the flag
is only ever set by Goose and its `unreachable!` is genuinely unreachable — the fix restores Goose's
own stated invariant rather than patching around it. The handle is also held as a strong `Arc` in a
`OnceLock` instead of a `Weak`, because `Drop for LlamaBackend` resets that flag *and* calls
`llama_backend_free()`; with two consumers the only sound rule is initialise once, never free.
Verified both orderings on the Mac with zero panics, including a real gemma-4-E2B chat turn running in
the same process as a loaded embedding model. Two mutation-tested tripwires hold it
(`no_giap_code_calls_llama_backend_init`, `the_backend_handle_is_held_strongly_and_never_freed`).

Second, a second embedding provider means a second WIDTH, and the `model_id` column that §5 names as
the guard belongs to a later phase. A mixed 384/768 store did not panic or warn — every comparison
returned `0.0`, which is a *valid score*, so the keyword fallbacks (gated on emptiness) never fired,
`search_similar` returned an arbitrary un-`ORDER BY`ed subset ranked as if judged, prompt injection
reverted to importance+recency, dedup stopped, and `run_backfill` (`embedding IS NULL`) could never
repair any of it. Fixed by filtering incomparable rows out of the candidate set in both adapters and
returning `None` rather than `Some(0.0)` in `topical_memories`. **The first two guards I wrote for
this passed with the fix removed** — vacuous, exactly the failure this checklist keeps recording —
and were rewritten until mutation testing failed them in both directions. The documented 384-dim
fallback model was itself the trap and is now 768, with a test that fails the build if any GGUF model
is ever added at fastembed's width.

Also fixed while here: `activate_model` wrote `embedding_provider = "embedding"`, a string matching no
provider arm, so activating any embedding model silently reverted a `gguf` pond to fastembed. Inert
while fastembed was the only implementation; not inert now.

---

**2026-08-14 — the prompt itself: judgment, covertness, and the leak that was never the prompt's fault.**

Brief was: get a small local model to run the best agent loop it can, stay covert about its inner
workings, judge for itself whether it needs a tool or a reasoning pass, and answer with the *result*
rather than an account of producing it. Five commits, `feat/v3-prompts`.

**The finding worth keeping.** `every_style_forbids_narrating_the_harness` carried a note saying the
rule was necessary but not sufficient — E2B and E4B both still leaked the word "goal" with it in
place — and that the second half was the nudge's own wording. That was right, and the cause is
sharper than "wording": `goose/crates/goose/src/agents/agent.rs` appends the completeness check as an
INVISIBLE USER MESSAGE reading `**Goal:** {goal}`, bolded, the noun repeated around it, positioned
after the system prompt, the entire tool schema and the whole history. It is the last thing in the
context before generation. Two more injections had the identical shape (the grind reminder, the
`/goal` kickoff) and nothing named them.

So the prompt's prohibition was never the trigger — it was the only counter-pressure, applied from
several thousand tokens away. `crates/pond-mcp-server/src/format.rs` had already written the law down
from an unrelated measurement on the same models: *a competing suggestion beats a buried one*. Fork
patch **seven** rewords all three; `pond-adapters-goose/src/goose_nudges.rs` pins the trigger by
`include_str!`ing the fork file, and lives in that crate rather than `pond-core` precisely because
`pond-core` must stay buildable without the submodule.

Only after that did the `lower.contains("goal")` assertion come out of the prompt guard. Keeping it
would have required the prompt to introduce a word the harness no longer says — making the prompt the
only place the model ever meets it, which is the pink-elephant version of the bug the rule exists to
catch. What replaces it is general rather than enumerated: every style says anything in angle brackets
is plumbing. The old list ("goal reminders, budgets, retries, system notes") had already gone stale —
`<tool-groups>` joined the envelope with PAI-8's tool selection and matches none of those categories,
so a model quoting it broke no rule the prompt stated.

**A guard that was measuring a pond nobody owns.** `v2_compact_static_prefix_within_token_budget`
rendered with `..Default::default()`: thinking OFF, zero devices, no vision. Production defaults
`thinking_mode` to "auto" (true for Gemma-4), households have devices, and E4B declares an mmproj. It
reported ~2,200 chars and passed while the prefix those devices receive was ~3,300 — the unreachable
fixture `turn_trimmer.rs` names by name. It now sweeps six *reachable* shapes; enumerated, not a 2^5
product, because `thinking_section_applies` and `vision_section_applies` both short-circuit on voice,
so a blind sweep would have reinstated the same trap one level down. Two budgets now, because one
number conflated the style's own verbosity (2400, which I control) with what a household's
configuration costs (3200) — a pond was being penalised for owning a lock.

**PAI-5 (thinking).** `<thinking>` only ever said when TO think. It now carries the counterpart
licence, phrased as a narrow exception adjacent to its obligation — never standalone, because this
repo has measured a detached permissive clause three times, most sharply `turn_budget_note`'s "pace
yourself" wording producing ZERO tool calls on a ten-item question. **NOT VERIFIED.** Prompting cannot
make adaptive thinking reliable (AdaptThink trains it; router approaches use a separate model), so
this is a bias and `thinking_mode` remains the switch.

**Still owed, and it is the part that decides.** Everything above is a claim about TEXT. Whether the
two judgment licences help or harm is an Orin question with a specific shape, and the obvious failure
mode is that a 2B model with permission to answer directly stops calling tools at all. The acceptance
criteria have to be fixed before the run: a MUST-CALL set (n>=12, each with a named expected tool)
whose tool-call rate must not fall, and a MUST-NOT-CALL set whose spurious-call rate must strictly
fall. `scripts/benchmark-dual-engine.sh` already has this shape — `run_test` takes `expect_tool` where
`"none"` means must-not-call — and needs repetitions, a template axis and tallying.
`scripts/pai_bench.py :: chat()` reads the SSE stream but drops `tool_call` events on the floor;
collecting them is three lines and every existing probe gains a tool trace. Hold `goal_check_enabled`
fixed across arms: it roughly doubles inferences per turn and is the largest confound. Do not ship on
a Mac run — both prior tool-calling regressions were measured on E2B.

**Also fixed, because the Prompts tab was quietly destructive.** `upsert_prompt_template` returned
`{"name","status":"ok"}` while the client typed it `Promise<PromptTemplate>` and read
`updated.content` — `undefined`, so a *successful* save blanked the editor, and the natural response
is Reset, which discards the edit for real. `description` was a `#[serde(default)] String`, so the
client's `{ content }` body wiped it on every save. Both mutation-checked.

That bug also poisoned the population for the v3 rollout question, which is why the answer is
`factory_version` (migration 0048) and a notice rather than adoption. Clearing `is_customized` where
content still matched an old factory string is migration 0035's settings adoption run BACKWARDS:
`DEFAULT_ADOPTIONS` moves a value only where `value = old_default` AND `is_user_set = 0`, because "a
row on its own proves nothing". `is_customized` IS this table's `is_user_set`.

**Interdependency check (2.2).** KV prefix: unmoved — `answer_contract` rides the user message, which
`prefix_hash` does not cover, and the trimmer strips it from prior turns so it never accumulates.
Preamble tokens: reduced in every reachable shape (balanced's Orin default 3,339 -> 2,642). Guest
scope: untouched. Approval: untouched. PAI-6: the subagent was the one thing that would have broken if
the rules had MOVED to the tail rather than being restated there — `orchestrator.rs` builds a child's
whole prompt from `base_system_prefix` plus its envelope and `child_user_message` has no
`<system-context>` — so the child takes `ANSWER_RULE` from the same constant, with a behavioural test.

---

**2026-08-13 — DuckDuckGo removed from the pond; Wolfram|Alpha in its place. Touches PAI-2 only.**

Not a PAI phase. It is recorded here because it moved two things this checklist owns — the egress
allowlist and a new keyed credential — and check 2.2 says to write down what a change does to the
other seven even when it belongs to none of them.

**What changed.** `giap-knowledge`'s `instant_answer` and `search_web`'s fallback were the pond's
only two DuckDuckGo callers; both are gone. `compute_answer` and `explore_computation` replace the
first, over Wolfram|Alpha's Full Results API, in a second `#[tool_router]` impl
(`pond-mcp-server/src/wolfram.rs`) composed onto the same `KnowledgeMcpServer`. Tool count 64 -> 65,
extension count unchanged at 17.

**Corrected 2026-08-14 on merge: the count is 66, not 65.** Two tools out and two in nets zero, so
the arithmetic above was right — against the tree the branch was looking at. That branch was 17
commits behind a `main` which had gained `giap-context__recall` in the meantime, so the merge
produced 66 while both sides believed 65, and `the_tool_inventory_parser_sees_the_tools_that_are_there`
failed on the integrated tree having passed on each side. This is the stale-base failure the review
procedure exists to catch, and it is worth recording because the guard did its job: the number is
derived from source in a test, so the merge could not quietly ship a wrong count the way the prose
did for months.

**Run against 2.2, item by item.**

- **Secret on `Settings`?** No. `WOLFRAM_APP_ID` is in the secret store via `secrets.rs`, the PAI-2
  P2 pattern, and reaches the UI only as a name in `Settings.tsx :: API_KEYS` — the same row shape as
  `GUARDIAN_API_KEY`. `GET /api/v1/settings` never sees it.
- **New egress point without `record_egress`?** No. Every call goes through
  `crate::http::traced_get_with`, which is the choke point. `wolframalpha.com` is on
  `KNOWN_PUBLIC_SUFFIXES`, so the list is back to 17 entries after losing `duckduckgo.com` the same
  day. Two line ranges cited in the PAI-2 document had rotted by ~230 lines and now name symbols.
- **A new way to leak a credential.** This is the one genuinely new risk and it is not one 2.2 lists,
  because until now no built-in tool carried its key in a **query string**. `reqwest`'s error
  `Display` includes the URL it failed on, so the naive `eprintln!("GET {url}")` that every other
  tool in this crate uses would have printed the AppID into the log on the happy path and into the
  tool result on the failure path. `redact_appid` covers both, and two tests hold it there.
- **Guest reaching personal data?** Yes, latently, and it was caught by a test rather than by
  design. The explore ids are short (`w3`) so a small model can echo them, which also makes them
  guessable — and the ring holds the user's own question text. It is now keyed by the engine session
  from the call's `_meta`, never by `current_session_id()`, which `session_meta.rs` documents as
  unusable for exactly this because four chat streams race that one cell. A call with no session in
  `_meta` is offered no ids at all rather than sharing a bucket, which is the `giap-draft` "default"
  bug the same file records.
- **Preamble tokens?** Net +1 tool schema, roughly +100 tokens per turn through the Gemma template.
  Paid deliberately: the pond had no way to compute anything, and `tool_selection_mode` is the lever
  that answers this properly for all 65.
- **KV prefix, blocking a turn on an LLM call, acting without approval?** None of the three. The
  suggestion chips in the card send a *message*, so the follow-up goes back through the engine and
  is audited and policy-checked; reaching the tool directly would have needed the knowledge tools on
  `DIRECT_DISPATCH_ALLOWLIST`, which grants them to every paired client and MCP App iframe at once.

**A consent question this does not answer.** Wolfram requires an AppID, so unlike Wikipedia or
DuckDuckGo the far end can attribute every question to an account. The egress classifier calls the
host `Public` because that is what `Public` already means on that list (`finnhub.io`, `gnews.io` and
`guardianapis.com` are all keyed), but "the pond was built to talk to this host" is not "this host
cannot profile the household". The tool is inert until someone enters a key, which is the whole of
the consent story today. Whether that is enough is a PAI-2 decision nobody has made.

**Gates run.** `cargo fmt --check` clean; `cargo test -p pond-mcp-server -p pond-core` green;
`cargo check -p pond-server -p pond-adapters-goose` green; 343 frontend tests green; `npm run build`
clean. **No live run** — nothing here touches a migration, a route, a handler or startup wiring, so
gate 11 does not apply. **Not benchmarked on the Orin**, and it should be before anyone calls the
in-chat card verified: `pai-bench` has no probe for it, and the one claim I cannot make from a
laptop is that a 2B model reliably echoes `w3` back into `explore_computation`. That is the entire
premise of the short-id design and it is measured nowhere.

**`search_web` disabled the same day, at Jerry's call.** Wolfram cannot stand in for it — its own
API docs are explicit that an uninterpretable query returns no pods and no links, only
`didyoumeans` / `tips` / `futuretopic`, because it indexes a curated dataset and not the web. And
`search_web` is declared LAST RESORT, so the traffic reaching it is exactly what the specific tools
already failed on: current events, local businesses, product pages. With the DuckDuckGo fallback
gone, SearXNG was its only backend and SearXNG is something the user has to run, so out of the box
the tool could only return a dead end while still costing a schema in every turn.

The disable is the absence of the `#[tool(...)]` attribute; the body is kept and kept compiling,
because a `#[cfg(feature)]` would hide it from the compiler and this checklist already records what
happens to code CI only ever `check`s. Consequences worked through rather than left: **seventeen
`format_no_results` call sites across four files were pointing at it**, and `format.rs`'s own doc
says never to name a tool that does not exist because a fabricated suggestion burns the model's one
retry. Financial misses now chain to `compute_answer` (Wolfram has currency, crypto and
public-company figures), country misses to Wikipedia, and the three that genuinely have no sibling —
barcodes, crowdsourced prices, books — say so with an empty alternatives list instead of inventing
one. `searxng_url` moved to `HEADLESS_BY_DESIGN` and its Settings row was deleted, because nothing
reads it any more and an input that cannot affect anything is the switches-that-were-not-switches
defect in a different costume.

**Two guards came out of it**, both mutation-checked. `every_suggested_alternative_is_a_tool_that_exists`
parses every `#[tool]` in the crate into an inventory and asserts each suggestion is in it — the
sibling guard only ever checked that a name was *well-formed*, which
`giap-discovery__search_web` still was. And `the_tool_inventory_parser_sees_the_tools_that_are_there`
pins the count, which is how I found that **AGENTS.md's "64 tools" had been wrong**: the number at
`HEAD` before any of this session's work was 65. Prose nobody checks drifts; that line is now
guarded and says so.

**A defect this batch shipped and then caught, worth the retelling.** `compute_answer` and
`explore_computation` were registered, compiled, covered by twenty-odd unit tests, and **not offered
to the model at all**. `giap-knowledge` is the first extension assembled from two `#[tool_router]`
impls composed with `+` in `new()`, and a bare `#[tool_handler]` resolves its router as
`Self::tool_router()` — the function generated for one impl block — so the composed field was
ignored and `list_tools` returned four tools instead of six. Nothing failed. `cargo check` was
green, the production binary built, every test of the parsing, rendering, session scoping and
redaction passed, because all of them called the functions directly.

The fix is `#[tool_handler(router = self.tool_router)]`. The lesson is the one section 2.1 already
states and this programme keeps re-learning: **a test that calls the function is not a test that the
system calls the function.** The guard that found it drives `ServerHandler::list_tools` and
`call_tool` on a real `RequestContext` (`both_tools_are_actually_exposed_by_the_server`,
`calling_the_tool_without_a_key_names_the_signup_page`), and it is paired with
`an_unknown_tool_name_is_refused_by_the_router` so "the tool is registered" cannot pass against a
router that accepts anything.

**Pre-existing, found while here, not fixed:** `cargo clippy -p pond-core --all-targets` fails on
`tests/egress_guard.rs :: ungated_egress_is_capped_and_shrinking` (`absurd_extreme_comparisons` —
`MAX_UNGATED` is now 0, so `len() <= 0` is always-or-never). CI's clippy step has no `--all-targets`,
so it is invisible there. Changing `<=` to `==` changes what the guard claims ("only shrinks" vs "is
exactly this"), so it is a deliberate call, not a lint fix.

---

**2026-08-14 (later) — the extension surface: a catalog with a working set, and four things that were inert only because the default is `"all"`.**

Brief was a redesign across seven axes — queuing, batching, precision, fault tolerance and
cross-recommendation, MCP-UI, extensibility with the builtins standardised, and "extensions
for everything". Plan at `.claude/plans/`, branch `feat/extensions-redesign`, seven commits.

**The number that drove the design.** GIAP ships **32 tools across 11 extensions**, 27 of them
offered by default, to gemma-4-E2B/E4B on an 8192-clamped prompt window, with
`tool_selection_mode` defaulting to `"all"`. It was 66 across 17 until 2026-09-10, when 20 tools
and then six whole groups were removed — see
[`prefill-cost-audit.md`](../../developer/prefill-cost-audit.md). Published benchmarks put tool-selection accuracy for *Haiku* below 90% between **10
and 15** tools; at 107 both large and small models fail outright. GIAP's own D-phase numbers
agree on cost (59 tools = 6,539 tok = 11.4 s TTFT; 17 = 2,386 = 4.0 s). "Extensions for
everything" and "precision" therefore pull against each other, and the resolution is the
design: a capability CATALOG with a small resident WORKING SET, so adding an extension grows
the catalog and not the prompt.

**PAI-1 — a guest was shown a menu of what had just been withheld from it.** The selection
stripped `groups_denied_to_guests()`, and then `dormant_groups_note` was built from
`registered_extensions()` — the full list. Dormant = registered ∖ selected, so `giap-memory`,
`giap-vision`, `giap-sensors`, `giap-audit`, `giap-context` and `giap-orchestrator` all
appeared under the sentence *"call enable_tool_group with its name and its tools become
available immediately"*. And `enable_group` checked catalog membership and registration only,
neither of which knows who is asking; `giap-toolkit` is deliberately not on the guest denylist.
Guest reads menu, takes item. There is now one `permitted` ceiling that both the menu and the
hatch read, applied to the candidates going *in* — which works because `select_groups` filters
the core set by `available`, contrary to a comment that had claimed otherwise for as long as
that filter existed.

**PAI-1 again — every third-party MCP server was outside the boundary.**
`subtract_guest_denied_tools` compared a prefix against a list of `giap-*` literals, so
`!denied.contains(&ext)` was structurally `true` for every user-added server. Now
default-denied, with engine plumbing (`platform`, `recipe`, `dynamic_task`) exempted by name —
a distinction the first version of the fix missed and an existing test caught.

**PAI-6 — under narrowing, most delegations get NO tools.** The child's ceiling came from the
parent's *loaded* groups; a parent holds ~4 core + 1 scored and three of the four cores are on
`groups_denied_to_subagents()`, so a research role asking for knowledge + news got `{}`. The
bound was wrong, not the arithmetic: the parent can enable any permitted group at will, so
`loaded` was never a boundary, only a position. Now bounded by the entitlement, through the
same guest subtraction. **This retired the plan's own Phase 2** — a child-scoped escape hatch
would have had nothing to do, because a child's allow-set *is* its whole grant.

**The one that would have made narrowing a lie on the Orin.** `group_embeddings` was
`OnceCell<Option<_>>`, so a failed first attempt was a value kept for the process lifetime.
The Jetson downloads its embedding model in a task `main.rs` deliberately does not await, so a
fresh install's first session embeds against a file that has not arrived — and then ran with
all 66 schemas in every prompt, for every session, while the trace said `mode = "relevant"`.
Now `get_or_try_init` plus a `tool_selection_widened` warning carrying `no_embedder` /
`embed_failed`.

**Not PAI, but found on the way and shipped alone and first:** `McpAppHost` rendered untrusted
MCP-App HTML as `srcDoc` with `allow-same-origin`. A srcdoc frame inherits the embedder's
origin, so the guest had `parent.localStorage` — where the session and refresh tokens live —
and could strip its own `sandbox` attribute off the parent DOM. Attacker-controlled the moment
any third-party server ships a `ui://` resource, with `tauri.conf.json` at `"csp": null` and no
CSP header on any route. One attribute value; the opaque origin it yields also satisfies
SEP-1865's different-origin MUST.

**Still owed, and each is blocked on something this machine cannot supply.**

*The flip itself.* Everything above is a prerequisite; `tool_selection_mode` still defaults to
`"all"`, and that default is what has been suppressing all four defects above. Flipping it is a
`DEFAULT_ADOPTIONS` migration, not a default change — a stored `"all"` outranks any default,
and `settings.rs:84` names this exact key. Its acceptance criteria are an Orin run with the
criteria fixed BEFORE it: a MUST-CALL set whose tool-call rate must not fall, a MUST-NOT-CALL
set whose spurious rate must not rise, and — the row that carries the real risk — a CROSS-GROUP
set asking whether the tool the model needed actually arrived. Selection is per-session, so the
mechanism that has to answer that row is re-scoring on topic drift, NOT the model rescuing
itself: this checklist already records a 2B declining to call `delegate` at all, and writing
`type` for a key that did not exist.

*MCP Apps.* `structuredContent` exists on rmcp 1.5.0 and is not the blocker. The blockers are
all 17 servers declaring `ProtocolVersion::V_2024_11_05`, no server declaring `resources`, and
`mcpui = false` because `GoosePlatform::GooseCli` was passed with `mcp_host_info: None` — goose
implements the whole hydration and GIAP never switched it on. `AgentConfig::with_mcp_host_info`
is a builder, so no fork patch; but hydration adds a `read_resource` round trip INSIDE the
tool-dispatch future, which on the Orin is per-call latency on the critical path and wants
measuring first. Note also that `extract_ui_hint` runs in the SSE layer, i.e. AFTER goose has
committed the full `[[[mcp-ui:…]]]` marker to the model's conversation — so `routes.rs`'s claim
that "the LLM only sees the clean text" is false, and 28 marker sites pay prompt tokens every
turn. That is the real argument for the migration.

*One-shot timers.* `tool_group.rs` advertises "Reminders, alarms, **timers**" and
`pond-voice/src/control.rs` documents "stop the kitchen timer". Neither is possible:
`SchedulerPort` is cron-only by type, and a fully-specified 6-field cron has no year field, so
`0 35 14 9 8 *` fires every 9 August — a ten-minute timer silently becomes an annual alarm.
`tokio-cron-scheduler` supports one-shot natively (`Job::new_one_shot_at_instant_async`); GIAP
only ever calls `Job::new_async(cron, …)`. Needs `fire_at` on the domain type and the port, a
migration, and self-delete-after-fire in the same transaction that writes the `ScheduleRun`.

**Interdependency check (2.2), run against all eight.** KV prefix: unmoved — nothing here
touches the static prefix, and the entitlement set is computed per turn without being sent.
Preamble tokens: unchanged this round (the reduction is the flip, which has not happened).
Guest scope: tightened twice. Approval: untouched. PAI-6: the delegation ceiling widened to the
entitlement, which grants nothing the parent could not already reach. PAI-2: the degradation
notes name a missing key but never its value, and `secret()` still returns `None`
indistinguishably for unset / unreadable / no-store.

**2026-09-10 — six tool groups deleted; PAI-1 P5 and PAI-6 P3 denylists shrank, PAI-2's outbound
gate is gone.**

- `giap-news`, `giap-audit`, `giap-finance`, `giap-discovery`, `giap-vision` and `giap-draft` were
  deleted to cut the prompt's tool payload: 46 registered tools to 32, 41 offered to 27, 19,193
  chars to 13,358 (~3,339 tok, 40.8% of the 8,192-token local prompt budget). Twenty individual
  tools had gone the same way a day earlier. See
  [`prefill-cost-audit.md`](../../developer/prefill-cost-audit.md).
- **PAI-2 regressed, and this is the entry that says so.** `giap-draft` was the outbound-action
  gate — the pond's only propose-then-confirm surface — and `giap-audit` was the only way to read
  the egress record back. Section 3.6 of
  [`02-privacy-and-security-guardrails.md`](02-privacy-and-security-guardrails.md) now carries the
  correction. The lock/alarm rule survives as prose on `set_device_state`'s description, which is a
  prompt-surface control rather than a mechanism. Treat P-outbound as **regressed, not satisfied**,
  until a replacement gate exists.
- **PAI-1 P5 / PAI-6 P3**: `groups_denied_to_guests()` lost `giap-draft`, `giap-audit` and
  `giap-vision`; `groups_denied_to_subagents()` lost its entries for the deleted actuating groups.
  Both properties still hold for the groups that remain, and both lists' vacuity controls were
  repaired — they had been asserting over groups that no longer existed, which passes for the wrong
  reason.
- A third `tool_selection_mode`, `"minimal"`, offers the toolkit hatch alone: 222 tokens, 2.7% of
  the prompt budget, against 40.8% for `"all"` and a 9.5% floor for `"relevant"`. Default is
  unchanged at `"all"`. `pond-mcp-server/src/toolkit.rs` pins the 4% ceiling as a test over the real
  serialized schemas.
- **Verification debt**: none of this has run on the Orin. The device is still on `71b45984` — the
  old prompt, the old prefill metric and all 41 tools. Every number above is a Mac measurement or a
  character count; the PAI-3 / PAI-4 re-verification the engine work owes is unaffected but still
  outstanding.

**2026-09-24 — PAI-5 P6: the reasoning was persisted and served, and dropped by the client.**

- The server half was right all along: `get_session_messages` sends `thinking: string[]` on
  every assistant row recorded with `persist_thinking` on, and the store's
  `sessionMessagesToMessages` reads `m.thinking` to refill the panel. Between them sits
  `PondApiClient.getSessionMessages`, which copies fields one at a time and never named `thinking`,
  so the refill always read `undefined`. Seen with the real client against a live scratch server:
  the wire row carried both passages, the mapped row carried none.
- **Why nothing failed.** P6's render tests in `Chat.test.tsx` mock `api.getSessionMessages`, so
  the payload they feed the store is already a mapped `SessionMessage`, and the one mapping that
  dropped the field never ran. The P6 section of the PAI-5 document recorded four seams; this was a
  fifth, and a mock that stands in for a port is exactly the place a seam goes missing. The
  standing lesson is PAI-1's in a new place: a fixture production could never produce proves
  nothing, and "already mapped" is a fixture shape the wire never produces.
- **Fixed, with two guards.** `thinking` is mapped, passed through as sent (absent stays absent,
  `[]` stays `[]`). `PondApiClient.test.ts` drives the real mapping from a raw fetch body captured
  over real HTTP, and asserts the mapped rows equal the wire rows, so any dropped field fails by
  name. And the mapped literal `satisfies Record<keyof SessionMessage, unknown>`, so a key added to
  the type and left out of the literal is a `tsc` error. Both mutation-tested: deleting the one
  line fails `tsc` with TS1360 and three of the new tests; restored byte-identical.
- **The same replay path had a second defect, outside PAI**: history images were handed to
  `<img src>` as bare attachment URLs, and that route is protected with a bearer-only middleware,
  so every one was a 401 on any pond without `POND_DEV_ALLOW_LOOPBACK`. The store now fetches the
  bytes through the client and shows them through object URLs it owns and revokes under the
  existing `ownedPreviews` rule. Recorded here because it is the same seam, and because the
  loopback bypass is the one setting under which neither defect could be seen.
- **Interdependency check (2.2), run against all eight.** KV prefix: unmoved — nothing here
  reaches a prompt, and `thinking_is_never_replayed.rs` still pins that the reasoning never does.
  Preamble tokens: none. `profile_id`: untouched, and no server code changed. Egress: none — the
  new request goes to the same pond, on the same protected route, with the token the client
  already holds. Secrets: none on `Settings`; the token stays in a header and is never put in a
  URL. Guest: no new read. The attachment route already served these bytes to any bearer that
  asked, and the client now asks for the images of a conversation it has just read. No turn
  blocks on anything: images load after the text. No side effects.
- **Worth knowing, not changed here:** neither `get_session_messages` nor
  `get_session_attachment` takes the caller's `Principal`, so any paired client can read any
  conversation, and its images, by id. That was true before this change and is still true
  after it; it belongs to PAI-1's session-identity binding and PAI-2's guardrails, not to a
  client fix.
- **Verification**: Mac only, 2026-09-24 — the fixed renderer in a browser against a scratch
  `pond-server` with the bypass OFF showed the history image and "Thought for a moment" expanding
  to both passages; the shipped build, same server, showed a broken image and no panel. Not run on
  the Orin; nothing here is device-specific, but PAI-5's `VERIFIED` is still only the token count.

**2026-09-24 -- picture support that sets itself up, and a switch for speculative decoding. Touches PAI-2 and PAI-3.**

- **What the household asked for.** "Out of the box picture processing with minimal user input": the
  files a model needs to look at a photo download and set themselves up with the model. Separately, a
  switch under Settings > Models that turns speculative decoding off. **The switch was built and then
  commented out the same day**: a parallel session took speculative decoding out of the llama.cpp
  engine (goose `743649d98`, "Jerry asked for speculative decoding to go"), so it had nothing left
  to control. The setting, its persistence, the gate, the per-turn reconcile and evict, both shells'
  controls and their tests are commented out rather than deleted, and so is the GIAP side of the
  drafter: its startup download and registration, the "not running" notice, `mtp_drafter.rs` (left
  out of the build) and the drafter field `apply_*_settings` stamped. The Orin window still charges
  the drafter's size, which keeps every window at the value it was measured at; giving E4B IQ4_XS
  those tokens back is a separate change that wants an Orin run.
- **What was actually broken, measured on both machines before any code.** The Orin's E2B encoder was
  636,790,074 of its 986,833,728 bytes (a transfer that ended early, stamped as ready because the only
  check was "the file is not empty"), so every E2B picture failed in the engine with goose's own
  "Settings > Local Inference" sentence, a screen GIAP does not have. The Orin's ACTIVE model,
  `gemma-4-E4B-it-qat-UD-Q4_K_XL`, could never take a picture: the encoder lookup split a name at its
  last dash, so no qat or UD spelling ever resolved. qat and non-qat encoders are DIFFERENT files with
  IDENTICAL byte sizes (the vision-to-text projector differs), so the obvious fix, one encoder per
  family, would have attached the wrong projector silently; identity is now the HF LFS sha256. The
  fetch restarted from byte zero on every attempt, followed redirects without gating them, was not
  resumable, pinned nothing, showed nothing, and a refused image turn was persisted although its
  message said it was not sent.
- **What landed.** Policy in pond-core: `vision_encoder` (a pinned table of five encoders keyed by
  family AND qat-ness, header and extent validation, a `.verified` sidecar so the ~1 GB hash runs
  once, `EncoderState` on the wire, the retry ladder, the stamp plan over every registry row naming a
  file, and every household sentence pinned by a test); `device_budget` (the Orin window arithmetic,
  moved rather than copied, plus an encoder term, the device-aware declaration, and
  `DEVICE_MEASURED_VISION`, which ships EMPTY); `Agent::vision_state` / `prepare_model`. Mechanism in
  adapters: pond-hf-cache refuses a short, overlong or mis-ranged transfer and
  keeps what resumes; the goose adapter fetches through it (serve process only), quarantines a bad
  file instead of deleting it, stamps or clears every row naming the GGUF before the engine loads,
  refuses pictures for providers that drop them, and rewrites a picture the engine could not read out
  of the session so one failure cannot poison the conversation. pond-api refuses an image turn before
  anything is saved (409 / 415), turns WebP into something the engine can decode, and serves
  `GET /models/vision-status`. Both desktop shells show where picture support stands, keep the draft
  and the photo when a turn is refused, and mark models that read pictures.
- **Interdependency check (2.2), run against all eight.** KV prefix: the `<vision>` section still
  follows the DECLARED capability, now device-aware and cached per file, so it never moves within a
  process. It moves ONCE, at the first turn after upgrade, for models whose declaration flipped: on the
  Mac the qat models and `gemma-4-12b-it` now declare it; on the Orin nothing declares it until an
  encoder is measured there, so E2B and E4B IQ4_XS lose it. Nothing in this change evicts a model slot
  now that the speculation switch is commented out. Preamble tokens:
  none added. `profile_id`: untouched. Egress: the encoder fetch left its raw `reqwest` (one gate on
  the first URL, redirects followed unseen) for pond-hf-cache's redirect-aware client, which gates
  every hop; `vision_encoder.rs` left `EGRESS_TRACKED` because it no longer sends, and the two
  serve-start fetches joined `DOWNLOAD_CALLS`. `network_mode` stays the only consent lever; a mode
  change mid-transfer pauses it and the household is told which setting blocked it and where. Secrets:
  none; no new `Settings` field ships (the switch's is commented out), and the HF token is read where
  it always was. Guest: no new read of
  personal data; the new route and fields describe models. Blocking a turn: nothing waits on the
  download or the hash; an image turn that cannot be served is refused up front and a text turn is
  untouched. Side effects without approval: one, deliberate and asked for -- a ~941 MB download starts
  by itself, with its size stated on the Download row and in the status line before and while it
  moves.
- **Deferred, with the reason.** E4B pictures on the Orin: with the BF16 encoder its window falls from
  16,384 to 2,048, and 62% of that file is an audio tower the engine cannot use; a vision-only encoder
  (or a `skip_audio` fork patch) is the unlock. Lazy encoder residency (a goose fork patch). goose's
  double-count of a resident encoder in `validate_and_compute_context`, which on the Orin could refuse
  cold text turns once an encoder is resident -- the reason E2B is not on the measured list without a
  measurement. HEIC from a Mac's Photos library. Non-Gemma vision families. Catalogue rows that point
  at repositories answering 401. The onboarding summary's "Model: Auto (downloads on first run)", which
  is not true. A persisted mark on an attachment the engine could not read.
- **Verification**: Mac only, 2026-09-24 -- the suites as the stages left them (pond-core 2,153,
  pond-api 524, pond-hf-cache 43, goose adapter 266, local-inference 42, desktop 1,018, Playwright
  `hub-models-wiring` 4/4), each fix mutation-tested where the stage could break it on purpose, and
  re-run after the switch was commented out (desktop 1,010); then, on the final tree, the CI fast
  suite (3,424 tests, 0 failed), the goose adapter and local-inference tests (300, 0 failed),
  clippy and the production `cargo check --all-targets`, all clean of new warnings.
  `scripts/live-test.sh` passed, 123 + 18 checks, including new ones: `GET /models/vision-status`
  answers, and a picture sent to a text-only local model is refused with 409 `vision_unsupported`
  and leaves no session or message row, before and after a restart. **End to end on real models**
  (a scratch pond, the Mac's GGUFs hard-linked): `gemma-4-E2B-it-qat-UD-Q4_K_XL` went downloading ->
  verifying -> ready as its OWN encoder (the qat one, sha256 `38b33846...`) arrived through the new
  fetcher, with no restart, and answered a synthetic picture "The background color is red and the
  shape in the middle is a circle."; `gemma-4-E2B-it-Q4_K_M` verified the encoder already on disk
  (44 s, no download) and answered "The background color is red."; a truncated copy under Network
  reach Offline was set aside as `.invalid` (never deleted), reported blocked with the household
  sentence, and a picture turn was refused with 409 before anything was saved. Not run on the Orin: the
  memory reading with E2B, its drafter and its encoder resident during the boot warm-up (the gate for
  putting E2B on `DEVICE_MEASURED_VISION`), `ENCODER_COMPUTE_MB`, and the PAI-3 re-run after the
  budget move.
