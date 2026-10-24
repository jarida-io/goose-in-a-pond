# Memory System

What the pond remembers about the people it talks to. Memories are rows in `memory_fragments`
(`pond_system.db`), each with a segment, importance, tier, decay state and lifecycle. A background
batch engine reads stored conversations and writes them; each turn reads the most relevant back
into context; a decay pass archives what has gone stale; an idle consolidation pass merges and
prunes. The decay formula is the adaptive half-life from
[boop-agent](https://github.com/raroque/boop-agent).

## Architecture

```
  chat turn ──> ChatService::persist_assistant_turn ──> sessions / session_messages
                (no memory work on the turn path)              │
                                                               │ inference lane: memory_extraction
                                                               │ (idle 15 min, embedder required)
                                                               ▼
                                         BatchExtractionService::run_pass
                                           one window per conversation, one model call each
                                           (LlmConversationExtractor)
                                           write gate: fact_defect, subject gate, dedup bands
                                           dated items ──> reminders ──> proposals
                                                               │
  save_memory (MCP) ──────────┐                                │
  POST /api/v1/memories ──────┴──────────────> memory_fragments <┘
                                                 │    │    │
  each turn: GooseAdapter ranks and injects <────┘    │    └──> inference lane: consolidation
  cleanup (every memory_cleanup_interval_hours) <─────┘         merge / prune / split / recategorize
```

Every memory write goes through `RedactingMemoryRepository`
(`pond-core/src/user_data/services/redacting_memory_repository.rs`), which strips
credential-shaped material at `RedactionLevel::Secrets` before storage and drops the embedding of
a row that held a secret.

## Data model

### Segments

| Segment | Default importance | Default tier (`MemorySegment::default_tier`) | Meaning |
|---------|-------------------:|-------------|---------|
| Correction | 0.9 | Long | The person fixed something that was wrong |
| Identity | 0.8 | Permanent | Who they are: role, home, circumstances |
| Preference | 0.7 | Long | Likes, dislikes, defaults, style |
| Relationship | 0.7 | Long | A person or pet they know |
| Routine | 0.65 | Long | A habit; only batch extraction can see one |
| Project | 0.6 | Long | Ongoing work or goal |
| Knowledge | 0.5 | Long | A fact not about the person |
| Context | 0.3 | Short | Transient situation |

Batch extraction does not use the segment default tier: every extracted memory is `Long` (see
[Kinds](#kinds-and-segments)).

### Tiers and lifecycle

| Tier | `decay_rate` | Behaviour |
|------|-------------:|-----------|
| Permanent | 0.00 | Never decays; cleanup skips it; consolidation never prunes it, and a merge or split involving it stays Permanent |
| Long | 0.01 | Decays slowly |
| Short | 0.10 | Decays fastest |

Lifecycle: `active` (recalled and injected), `archived` (hidden from every search, kept in the
table), `merged` (superseded; `superseded_by` names the replacement).

Other columns: `profile_id` (the member it belongs to; `NULL` means household), `session_id`,
`source` (`extraction`, `mcp_tool`, `api`, ...), `tags`, `embedding` (BLOB), `access_count`,
`last_accessed_at`, and `corrects` (for a correction, the claim it overturns, so consolidation
cannot revert it).

## Batch extraction

`BatchExtractionService` (`pond-core/src/user_data/services/memory_extraction.rs`) is the only
code that extracts memories. The chat handlers persist turns with plain `persist_assistant_turn`;
`crates/pond-api/tests/stream_handler_parity.rs` (`neither_stream_handler_extracts_inline`) fails
the build if either handler extracts inline.

### When a pass runs

The loop in `pond-server/src/main.rs` polls every 60 s (first poll 60 s after start) and asks the
[inference lane](#the-inference-lane) for the `memory_extraction` slot. It is spawned only when an
embedding provider exists at startup; without one nothing is extracted, and the pass reports
`blocked_on = "no_embedder"`. A pass needs all of:

- `memory_extraction_enabled` and an embedder that has not failed in the last 300 s
  (`EMBED_FAILURE_COOLDOWN_SECS`);
- real user activity since the process started (HTTP routes, or a human session's `updated_at`,
  which covers the out-of-process voice child);
- `memory_extraction_idle_secs` (900) of quiet;
- `memory_extraction_interval_secs` (60) since the last pass.

Settings are re-read every tick. In-process activity during a pass cancels it between windows, so
activity costs at most one window's inference.

### What a pass reads

- Conversations: every human session (`SessionOrigin::is_human`; the scheduler's `sched-`
  sessions and the proposal session are skipped). The most recently active conversation goes
  first, then the backlog with never-examined conversations at the front, oldest first.
- Per conversation, one window: messages after the cursor (`sessions.extracted_through_id`,
  migration `0056`), up to `memory_extraction_window_messages` (20) and `EXTRACTION_WINDOW_CHARS`
  (6000), cut to end on an assistant reply. Fewer than two new messages is skipped.
- Up to `memory_extraction_sessions_per_pass` (3) model calls per pass, and at most
  `EXTRACTION_PASS_MAX_SECS` (300) of wall clock.
- Who the window is about: `resolve_window_subject` names the member from the session's
  `SessionIdentity`. With no identity, a single-member pond uses `settings.user_name` (or "the
  user"); on a multi-member pond the conversation is not mined, only counted
  (`sessions_unnameable`), because filing one member's habits under another's name is worse than
  losing them.
- Up to 8 known memories (`KNOWN_MEMORIES_SHOWN`, 800 chars) about that subject, ranked as turn
  injection ranks them, so the model builds on them instead of restating them.

The cursor advances after each window. A watermark naming a deleted message resets the cursor and
the conversation is re-walked; `already_mined` skips windows that already wrote rows. A reply with
no parseable JSON counts an attempt; after `MAX_PARSE_ATTEMPTS` (3) the walk steps past the window.
A window whose first exchange alone exceeds the budget is stepped past unread.

### The extraction prompt

`LlmConversationExtractor` (`pond-server/src/conversation_extractor.rs`) sends one system prompt
and one user message (known memories, then the conversation) per window, within
`EXTRACTION_PROMPT_BUDGET_TOKENS` (2400). The rendered system prompt is held under
`EXTRACTION_PROMPT_CEILING` (2300 chars, enforced by a test). Its opening differs from every chat
prompt so Goose's prefix reuse does not overwrite the chat's cached prefix. It carries no worked
example, because small models copy examples. The model answers
`{"memories":[{"note","kind"}],"reminders":[{"about","when"}]}`, at most
`memory_extraction_max_facts` (3) memories per window.

### Kinds and segments

The model chooses from a closed five-value catalogue (`MemoryKind`,
`pond-core/src/user_data/ports/conversation_extractor.rs`); an unknown label is rejected and
counted, never defaulted.

| Kind | Stored segment | Importance |
|------|----------------|-----------:|
| relationship | Relationship | 0.7 |
| preference | Preference | 0.7 |
| routine | Routine | 0.65 |
| correction | Correction | 0.9 |
| context | Identity | 0.8 |

All five are stored in the `Long` tier. `context` maps to `Identity`, not to the store's
transient `Context` segment, and is not `Permanent`, because a household's circumstances change.

### The write gate

Each candidate is normalised (`normalise_fact_content`: whitespace collapsed, an invented label
prefix such as "Active Project:" dropped), then:

1. **Quality gate**: `fact_defect()` refuses it outright (below).
2. **Subject gate**: an Identity, Relationship, Preference or Routine candidate that never names
   the subject (`names_subject`: "the user" or the member's name) is demoted to `Knowledge`
   (importance 0.5), not rejected.
3. **Bands**: compared with what the subject's store already holds (below). `Same` and `Related`
   are dropped and the id they lost to is recorded; `New` and `Unscored` are written.
4. **Write**, unless `memory_extraction_mode` is `shadow`. The banding vector becomes the row's
   embedding.

Every window logs a `memory_events` row (`event_kind = extracted`, `memory_id = window:<id>`) with
counts of what was offered, kept, refused, dated, demoted and dropped; each stored memory logs its
own row with its window, kind and band. Fact text never reaches the INFO log.

`memory_extraction_mode` is `write` by default; `shadow` bands and logs but writes nothing;
`reinforce` currently behaves as `write` (a match is still dropped). An unrecognised value reads
as `shadow`.

### Fact quality gate

A stored memory is injected into the assistant's context many turns later, with none of the
conversation that produced it. Content that only made sense inside that conversation misleads.
`fact_defect()` in `pond-core/src/user_data/domain/memory.rs` refuses, in this order:

| Defect | Example | Rule |
|--------|---------|------|
| `TooShort` | "Tea." | fewer than `MIN_FACT_CONTENT_LEN` (8) characters |
| `FirstPerson` | "My mom's name is Florence." | "I / me / my / we / our" and contractions ("I'm", "We're"); in the assistant's context "my mother" reads as the assistant's |
| `UnresolvedReference` | "The user's mother lives in the latter city." | "the latter"/"the former" without two named antecedents, a non-expletive "there" or a deictic ("that place") with none, or a sentence opening on a bare pronoun |
| `EchoedExample` | (none today) | an exact copy of a prompt example; `EXTRACTION_EXAMPLE_FACTS` is empty while the prompt has none |
| `CalendarDate` | "The user has a dentist appointment next Tuesday." | a one-off date or time (`carries_calendar_date`); a habit's recurring timing ("every Saturday") is kept |
| `Fragment` | "The user's cat is called" | ends on a word that leads into something missing |

The gate is deliberately hard to trip. A refused fact is lost, so every rule is anchored to a
token pattern a well-formed third-person sentence cannot produce, and each has escapes: "The
user's therapist" is not "there", "lives in the US" is not "us", "Type I diabetes" is not "I",
"There is a spare key" is the expletive, "grew up in the former Yugoslavia" names its referent,
and "moved from Nairobi to Kisumu and prefers the latter" resolves inside its own sentence.
Defective facts are not repaired: rewriting "I like" into "The user likes", or inventing the
referent of "the latter", is a judgement nothing has at write time, and a wrong repair outlives
the conversation that could have corrected it. The prompt carries the same rules, so compliance
is the common case and the gate is the backstop.

Two rules are deliberately looser than they could be, because each tighter version destroyed
more real facts than the imprecision it removed:

- **"mine" is not a first-person marker.** It is a noun ("a coal mine") more often than a
  pronoun, and no rule separated the two cleanly in both directions. The cost is that "That
  laptop is mine now." is stored.
- **An antecedent only has to precede its anaphor.** Proper nouns before the deictic are counted
  positionally and nothing else about them is inspected. Requiring a place antecedent for
  "there" or "that city" destroyed facts whose place arrives through a copula ("The user's home
  town is Kisumu and his parents still live there"). "the latter"/"the former" still need
  `CONTRASTIVE_ANTECEDENTS` (2) because they select between two. The residual imprecision: one
  earlier name of any kind resolves "that place".

### Dates become reminders

A `CalendarDate` note is refused as a memory because, read back months later, a dated fact
misleads. When `memory_date_proposals_enabled` (default true) and the window's last message is
under `REMINDER_MAX_AGE_DAYS` (7) old, the model is also asked for reminders: `about` plus
`when_said`, the person's own words about the timing, never a parsed date. They land in the
`reminders` table (migration `0057`, deduplicated on `(window_id, about_key)`). After each pass,
`promote_pending_reminders` turns up to 50 pending reminders into proposals; one with no member to
address stays `pending`. `GET /api/v1/reminders` lists them.

### Dedup bands

`dispose_candidates` scores each candidate against the subject's own store (another member's
identical row is a different person's fact):

| Band | Test | Outcome |
|------|------|---------|
| Same | lexical duplicate of one of the 50 most recent rows (`DEDUP_RECENT_WINDOW`, plus rows this window wrote), or cosine to the nearest of 5 neighbours at or above `memory_reinforce_threshold` (0.94) | dropped |
| Related | cosine at or above `memory_relate_threshold` (0.78) | dropped |
| New | cosine below 0.78 | written |
| Unscored | the embed failed or no neighbour has a vector | written |

The settings source marks both thresholds as unmeasured defaults; `shadow` mode exists to measure
them on real history.

The lexical test is `is_duplicate_content()` in `memory_relevance.rs`, layered because a token
measure alone is blind to meaning-bearing differences:

1. **Polarity**: opposite polarity is never a duplicate ("is happy" / "is not happy").
2. **Argument order**: "prefers dark mode over light mode" and "prefers light mode over dark mode"
   have identical token sets. `is_argument_swap()` splits both at a shared
   `ORDER_SENSITIVE_MARKERS` connective (over, than, to, from, instead, rather, before, after,
   versus ...) and checks whether the arguments crossed in both directions. Copulas are not on
   the list: "Florence is the user's mother" and "The user's mother is Florence" are one fact.
3. **Substring containment**, case-insensitive.
4. **Token overlap**: content words after possessive stripping, crude singularisation, kinship
   aliasing (mom to mother) and a dedup stopword list that also drops "user". A duplicate needs
   Jaccard at least `LEXICAL_DEDUP_JACCARD` (0.45) **and** containment over the shorter side at
   least `LEXICAL_DEDUP_CONTAINMENT` (0.8), with at least 3 content words on each side.

Both floors are required: Jaccard alone misses a short restatement of a long fact; containment
alone merges "prefers dark mode" with "prefers dark roast coffee". "mother lives in Kisumu" against
"mother lives in Nairobi" clears Jaccard but fails containment, which is right: contradictions are
both stored and left to consolidation.

### Corrections are never deduplicated

A correction restates the claim it overturns in almost the same words, which both the lexical and
the cosine test score as a duplicate. Dropping it would leave the stale row standing, so the store
would assert what the person just denied. A `Correction` candidate therefore skips the drop and is
written; both rows sit in the store and consolidation, which never prunes a correction, retires
the old one.

### Status

`GET /api/v1/memories/extraction-status` returns `ExtractionEngineStatus`: mode, when the last
pass finished, windows read, memories written, dates refused and lost, reminders written and lost,
unattributed conversations, and `blocked_on` (`no_embedder`, `no_provider`, `provider_error`,
`unnameable_subject`, `session_list_failed`) so a stalled engine does not look idle. It resets on
restart.

## Other writers

| Writer | Gate | Owner | Embedding |
|--------|------|-------|-----------|
| `save_memory` MCP tool (`giap-memory`) | none; `auto_classify_segment()` picks the segment when none is given | household (`profile_id` NULL) | embedded at write when a provider exists |
| `POST /api/v1/memories` | none | household | none at write; filled by the embedding sweep |

`auto_classify_segment()` (`pond-mcp-server/src/memory.rs`) is a keyword heuristic used only by
`save_memory`: "actually / no, / that's wrong" gives Correction, "my name is / I live in / I work
at" Identity, "my wife / my friend / my boss" Relationship, "I prefer / I like / my favourite"
Preference, "working on / my project / deadline" Project, "right now / currently" Context,
anything else Knowledge. `save_memory` also takes `supersedes` (ids marked `merged`) and
`corrects`.

Rows written without a vector are embedded by the index-maintenance lane job
(`run_memory_embedding_sweep`, `pond-core/src/context/index_maintenance.rs`) and by a backfill
spawned at startup (`memory_relevance::run_backfill`, then `run_dimension_repair` for rows
embedded by another model).

## Recall and injection

Each turn, `GooseAdapter` (`pond-adapters-goose/src/goose_agent.rs`) builds the memory block when
`agent_memory_inject` is on (default true) and the turn's `ProfileScope` allows personal data, so an
unidentified speaker gets none:

1. Candidates: the most recent rows plus topical ones (vector search with `embed_query`, or a
   keyword search via `keyword_terms` when there is no embedder), from a pool of
   `max(agent_memory_limit * 8, 40)`, all within the turn's scope.
2. Rank with `rank_by_relevance`: `0.5 * similarity + 0.3 * importance + 0.2 * recency`, recency
   halving every 14 days from `created_at` (`RECENCY_HALF_LIFE_DAYS`).
3. Keep the context profile's `max_memory_fragments` (3 to 15, by context size,
   `pond-core/src/models/services/context/context_budget.rs`) within its `memory_token_budget`.
4. Send only lines not already shown earlier in the conversation, inside the user message's
   `<system-context>`, which keeps the system-prompt prefix stable for KV reuse.
5. `record_access` on each injected memory, which feeds decay.

Only active rows are searched; archived and merged rows are never recalled.

`recall_memories` (MCP) searches the whole household: vector search when a query and embedder are
available, keyword fallback otherwise, recency order without a query (default limit 10). It
records access and logs a `recalled` event per result.

## Decay and cleanup

`memory_cleanup::effective_score` (`pond-core/src/user_data/services/memory_cleanup.rs`):

```
adaptive_half_life = memory_decay_base_half_life_days * (1 + importance)
lambda             = (ln 2 / adaptive_half_life) * memory_decay_beta * (1 + decay_rate)
score              = importance * exp(-lambda * days_since_access) * (1 + ln(access_count + 1) * 0.1)
```

clamped to `[0, 1]`. `days_since_access` counts from `last_accessed_at`, or `created_at` if never
accessed. Permanent rows return their importance unchanged.

`run_cleanup` scores the newest 1000 active rows that have an importance. Below
`memory_archive_threshold` (0.15) a row is archived and logged `archived`; below
`memory_prune_threshold` (0.05) it is also archived, but logged `pruned`. Nothing is deleted.
Empty-content rows are always pruned. With the defaults (half-life 11.25 days, beta 0.8) a memory
that is never recalled archives after roughly:

| Segment (importance, decay_rate) | Days to archive |
|----------------------------------|----------------:|
| Correction (0.9, 0.01) | 68 |
| Preference, Relationship (0.7, 0.01) | 53 |
| Routine (0.65, 0.01) | 49 |
| Knowledge (0.5, 0.01) | 36 |
| Context (0.3, 0.10) | 17 |

Cleanup is plain SQL, so it runs on its own timer, not the inference lane. The loop is spawned at
startup only if `memory_cleanup_enabled` is true, and its interval and thresholds are read once at
startup: changing them needs a restart.

## Consolidation

The only pass that removes what extraction got wrong. Two modes, `memory_consolidation_mode`:

- **`single`** (default): one LLM call (`pond-server/src/llm_memory_consolidator.rs`); every
  proposal is applied, and the audit trail records each action as accepted without review.
- **`adversarial`**: three sequential calls (`pond-server/src/three_stage_consolidator.rs`),
  cancellable between stages: a Proposer proposes `Merge` / `Prune` / `Split` / `Recategorize`, an
  Adversary argues against destructive or lossy changes, a Judge rules; only accepted actions are
  applied.

Both modes apply through `memory_consolidation::apply_actions`, so the guards cannot drift: a
`Correction` is never pruned, a merge involving one is forced to `segment = Correction` and carries
`corrects` forward (a split gives it to the first `Correction` entry), a replacement that fails to
insert aborts before its sources are superseded, and a `Permanent` row is never pruned while a
merge or split involving one stays `Permanent`. A consolidation prune archives the row. Every
lifecycle change is written to `memory_events`; each run is recorded in `consolidation_runs` with
its mode.

**Batch**: `select_batch` takes up to `memory_consolidation_batch_size` (50) segmented rows,
oldest first, because duplicates cluster in time. Fewer than `MIN_MEMORIES_TO_CONSOLIDATE` (6)
skips the run. What does not fit is logged as deferred.

**Trigger**: the `consolidation` lane job, polled every 60 s. It needs
`memory_consolidation_enabled` (default true), activity since start, `INACTIVITY_THRESHOLD_SECS`
(15 min) of quiet, and `memory_consolidation_interval_hours` (24) since the last run. Enable,
mode, interval and batch size are re-read every tick.

**Cancellation**: `AppState::note_user_activity`, called by routes that start user work, cancels
an in-flight run directly. A watcher also samples the in-process clock every 500 ms and human
session activity in the database every 2 s, which catches the out-of-process voice child. A stage
already inside an LLM call finishes first.

`POST /api/v1/memory/consolidate` runs a pass immediately and streams `ConsolidationEvent`s over
SSE (for the desktop banner); it refuses when consolidation is disabled and does not wait for the
lane. `POST /api/v1/memory/consolidate/stop` cancels.

Known gaps (also tracked in [context-and-reasoning-roadmap.md](./context-and-reasoning-roadmap.md)):

- The batch window does not rotate: on a store larger than the batch, newer rows are not reached
  until the oldest are acted on.
- The 15-minute threshold is a constant, not a setting.
- An attempt spends the interval even when cancelled or skipped for too few memories; on a
  device used every evening this can starve consolidation. Retrying at the next idle window is
  what made an earlier loop churn, so the trade-off is deliberate.

## The inference lane

Background jobs that spend inference share one slot (`pond-core/src/user_data/services/inference_lane.rs`,
runtime in `pond-server/src/inference_lane_runner.rs`), because concurrent jobs on the single
resident model evict each other's KV cache. At most one runs at a time; the least recently run
eligible job wins, ties going to declaration order. Each job has its own enable toggle, interval
floor and quiet requirement, and waits for real user activity since boot unless it is marked
exempt (`JobState::exempt_from_activity_gate`). Last-run times
persist in `lane_job_runs` (migration `0059`) so a restart does not make every job look starved.

| Job (wire name) | Memory relevance |
|-----------------|------------------|
| `compaction` | none |
| `consolidation` | merges and prunes memories |
| `titling` | none |
| `proactive_review` | none |
| `summary_refresh` | none |
| `index_maintenance` | embeds memory rows that have no vector |
| `memory_extraction` | writes memories and reminders |
| `suggestion_generation` | composes Home suggestions from memories into `suggestion_queue` (migration `0058`), gated by `suggestion_generation_enabled` |

`GET /api/v1/lane` reports each job's state; `POST /api/v1/lane/jobs/{job}/run` runs one now,
waiving its quiet period, interval floor and activity gate.

## MCP tools (`giap-memory`)

| Tool | Description |
|------|-------------|
| `recall_memories` | Query-based (semantic, keyword fallback) or recent recall across the household; returns segment, importance and ids; records access |
| `save_memory` | Save with optional segment, importance, tier, tags, `supersedes`, `corrects`; auto-classifies when segment is omitted |
| `forget_memory` | Delete by id, or by exact content match |

## REST API

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/memories` | The 50 most recent active memories, household-wide |
| POST | `/api/v1/memories` | Save `content`, optional `tags`, `source` (default `api`), `segment`, `importance`, `tier`; embedded at write |
| PUT | `/api/v1/memories/{id}` | Reword, change tier, or both; absent fields are kept, and new words' vector replaces the old |
| DELETE | `/api/v1/memories/{id}` | Delete |
| GET | `/api/v1/memories/extraction-status` | Batch engine status |
| GET | `/api/v1/reminders` | Reminders captured from conversations |
| POST | `/api/v1/reminders/{id}/dismiss` | Dismiss a reminder |
| POST | `/api/v1/memory/consolidate` | Run consolidation now (SSE) |
| POST | `/api/v1/memory/consolidate/stop` | Cancel a running consolidation |
| GET | `/api/v1/lane` | Inference-lane job status |
| POST | `/api/v1/lane/jobs/{job}/run` | Run one lane job now |

`POST` and `PUT /api/v1/memories` embed with the same provider and index the extraction path
uses. A row stored without a vector (the embedder failed, or the redactor dropped a vector of a
credential) is embedded later by the boot backfill or by the index maintenance sweep, which runs in
household quiet.

## Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `agent_memory_inject` | true | Inject ranked memories into each turn |
| `agent_memory_limit` | 5 | Sizes the injection candidate pool (`max(limit * 8, 40)`); the injected count comes from the context profile |
| `memory_extraction_enabled` | true | Run batch extraction |
| `memory_extraction_mode` | `write` | `shadow` / `write` / `reinforce` (internal, not a user control) |
| `memory_extraction_sessions_per_pass` | 3 | Model calls (windows) per pass |
| `memory_extraction_window_messages` | 20 | Messages per window |
| `memory_extraction_idle_secs` | 900 | Quiet before a pass |
| `memory_extraction_interval_secs` | 60 | Minimum gap between passes |
| `memory_extraction_max_facts` | 3 | Memories per window |
| `memory_reinforce_threshold` | 0.94 | Cosine for the Same band |
| `memory_relate_threshold` | 0.78 | Cosine for the Related band |
| `memory_date_proposals_enabled` | true | Ask for reminders from recent windows |
| `memory_extraction_first_pass_at` | empty | Meant to record the first completed pass; nothing writes it yet |
| `memory_cleanup_enabled` | true | Run decay cleanup (read at startup) |
| `memory_cleanup_interval_hours` | 6 | Hours between cleanup runs (read at startup) |
| `memory_decay_base_half_life_days` | 11.25 | Base half-life before importance scaling |
| `memory_decay_beta` | 0.8 | Decay steepness |
| `memory_archive_threshold` | 0.15 | Score below which a row is archived |
| `memory_prune_threshold` | 0.05 | Score below which a row is archived and logged as pruned |
| `memory_consolidation_enabled` | true | Run idle consolidation |
| `memory_consolidation_mode` | `single` | `single` or `adversarial` |
| `memory_consolidation_interval_hours` | 24 | Minimum gap between runs |
| `memory_consolidation_batch_size` | 50 | Rows per run (floor 6) |
| `suggestion_generation_enabled` | true | Compose suggestions from memories |
| `embedding_provider` | `fastembed` | `fastembed`, `gguf` or `none`; `none` stops batch extraction |
| `memory_graph_enabled` | false | Stored, but nothing reads it: graph retrieval over `memory_edges` is not wired |

## Database

All in `pond_system.db`; migrations in `crates/pond-infra/migrations/system/`.

| Migration | Adds |
|-----------|------|
| `0005_memory.sql` | `memory_fragments` (id, profile_id, session_id, content, embedding, source, tags, created_at) |
| `0015_memory_segments.sql` | segment, importance, tier, decay_rate, access_count, last_accessed_at, lifecycle, superseded_by |
| `0017_memory_edges.sql` | `memory_edges` (causal graph; no production writer) |
| `0019_memory_corrects.sql` | `corrects` |
| `0020_memory_events.sql` | `memory_events` audit log |
| `0021_consolidation_runs.sql` | `consolidation_runs` |
| `0036_enable_memory_consolidation.sql` | turns consolidation on where the stored value was still the old default |
| `0056_session_extraction_cursor.sql` | `sessions.extracted_through_id`, `extracted_at`, `extraction_attempts` |
| `0057_reminders.sql` | `reminders` |
| `0058_suggestion_queue.sql` | `suggestion_queue` |
| `0059_lane_job_runs.sql` | `lane_job_runs` |
