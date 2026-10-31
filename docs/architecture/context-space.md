# The context space

One store, one multimodal index and a set of shipped agents for everything a pond learns about its
household, from its phones, its home and its conversations. The model and the household search the
same space, and each sees only what its principal may see.

**Status: DESIGNED 2026-10-28, re-planned 2026-10-30, storage chosen and P0 measured 2026-10-31**
against `main` at `0309bf0d`. The re-plan builds on what landed on 28 and 29 October, summarised in
[changes-2026-10-28-29.md](./changes-2026-10-28-29.md): long tasks, agents as recipes, deep
research, the line, My Pond and `agamemnon`. The context, memory and index code is unchanged since
the first draft. No code has changed. This design replaces memories, summaries, the index,
suggestions and proactive review; section 3 lists what goes. It cuts across PAI-1, -2, -3, -6, -7
and -8, so [the PAI checklist](./pai/00-checklist.md) governs every phase.

---

## 0. Rules

1. **Counter-phase.** Indexing and context agents run only when the pond is not serving the
   household: no turn in flight, no voice session, no member's long task running. A turn stops them
   within one batch. The one exception is the query embed a turn needs for its own retrieval.
2. **Deny by default.** Retrieval filters by the caller's principal in SQL before anything is
   scored. A guest sees nothing. An agent reads as the member it works for.
3. **Propose, do not act** (PAI-7). Agents write items and proposals. A member's approval is what
   turns a proposal into an action.
4. **Personal data stays in the household.** Phones send to their own pond and nowhere else. Push
   wakes carry no content. A web lookup never carries a person's name, health or diet, as research
   steps already promise.
5. **The KV prefix is untouched.** Per-turn context rides `<system-context>`.
6. **Every derived item names its sources.** Deleting a source deletes what was derived only from
   it, and destroying a media item's key destroys its bytes.
7. **Every number is an Orin measurement or says it is not.**

---

## 1. What it builds on

Most of what the first draft proposed to build has since landed in another form. The context space
uses it rather than adding a second one.

| Landed (or open) | The context space uses it for |
|---|---|
| **Agents as recipes** (PAI-6 section 9): `agent_recipes` rows with a `giap_role` block of activations (`schedule`, `event`, `delegated`, `task_step`, `agent`), a fixed hook catalog, and a compiled prefill per agent | Every context agent is a shipped recipe. There is no new agent framework. Section 7 adds two activations and three hooks to the catalog |
| **Suggested agents** (P19): shipped templates offered as cards, none running until a member turns it on | Understanding, Profile and Proposal ship this way |
| **Long tasks** (PAI-6 section 8): a planner, workers and a verifier, with artifacts scoped to the owner | Planning *is* this: the composer role writes itineraries and a calendar artifact. A finished task's report and events become items, so they can be found later |
| **The line and the lane's order** (sections 8.9 and 8.10): compaction, then a member's long task, then chores; a chore that has waited 15 minutes gets one turn; a held subagent permit counts as a turn in flight; 30 s of household quiet before a task starts | Context agents are chores. The new embedding slot yields to anything ahead of chores |
| **Helpers that read the house** (`HOUSEHOLD_READ_TOOLS`, #717, open) | `search_context` joins that list, so a research step reads the household's own context instead of searching the web for it |
| **My Pond and the admin card** (`pond-administrator.md`): the nano has an OP-TEE firmware TPM with `tpm2-tools`, and the admin card's recovery copy is sealed to it | The data key is sealed the same way, and the admin card becomes its recovery key (section 9) |
| **Per-phone keys** (#715, open), held behind Face ID or a fingerprint | The same pattern holds the phone's outbox key |
| **`agamemnon`**: the pond as a locked account, its data in `/var/lib/goose-in-a-pond` (`0750`) | That directory is what the encrypted volume holds |
| **Household roles** (#707, open): Owner, Member, Guest, Developer | Who sees household items, and who may connect or remove a source |
| **Phone screens** for agents and prefills (goose-on-the-go #198), My Pond (#205) and the queue (#204) | Context agents show there with their state. Sources and consent go beside them |

---

## 2. Why one space

Today five systems hold what the pond knows, each with its own writer, scope filter, deletion path
and lane job:

- memories (`memory_fragments`, batch extraction, consolidation);
- conversation summaries (`sessions.rolling_summary`);
- an index over three corpora (`Corpus::{Memory, Context, Summary}`);
- suggestions (`suggestion_queue`);
- proactive review (`drafts`).

Long tasks have since added a sixth, artifacts, which is good working memory for a task but
unsearchable once the task ends.

Measured on the nano on 2026-10-28:

- The embedder is fastembed `all-MiniLM-L6-v2` (384 dimensions, English-only).
- 25 memory vectors and 40 summary vectors exist, and nothing else is indexed.
- A 146 MB nomic GGUF sits on disk and is never loaded.
- Context items are reachable only through `giap-context`, which ships off.

**What changes:**

- Every piece of knowledge gets an **item** header, with:
  - an owner and an audience;
  - its provenance;
  - one vector in one multimodal space.
- Memories, summaries, mail, photos, camera events, suggestions and finished reports are item
  *kinds*. Each keeps its own table for the fields only it has (section 4.1).
- Each kind declares its lifecycle (section 4.2), instead of inheriting it from the table it sits in.

This keeps the personal-context index's "one index, separate stores" where it counted: a kind's own
fields stay in its own table, so nothing is merged that cannot be unmerged. The header unifies what
each system got wrong on its own: scope, provenance, lifecycle and deletion. The index's other
decisions stand:

- vectors live in `pond_vectors.db` and hold no text;
- staleness is a `LEFT JOIN`, not a queue;
- every vector carries its `model_id`;
- faces stay out.

---

## 3. What this replaces

| Today | Becomes | Phase |
|---|---|---|
| Batch extraction (`BatchExtractionService`, lane job `MemoryExtraction`), summary refresh, titling | The Conversation agent, with contextual extraction (7.3) | P3 |
| Dedup and ranking (`memory_relevance.rs`) | One ranker in the unified retriever; dedup moves to Consolidation, with a threshold measured on EmbeddingGemma 2 | P3 |
| Consolidation (lane job, plus `POST /memory/consolidate`, which bypasses the lane) | The Consolidation agent, adversarial by default (7.3), lane only | P3 |
| Cleanup and decay (a 6-hour SQL timer) | The lifecycle per kind, enforced by one sweep | P1 |
| Per-turn recall (`topical_memories`), by brute-force cosine over `memory_fragments.embedding` | The unified retriever, over the index | P3 |
| Three corpora, plus vectors stored on source rows with no `model_id` | One index keyed by item, holding every vector | P1, P2 |
| Startup backfill (no idle gate) and the `IndexMaintenance` lane job | The embedding slot (6.2) | P2 |
| `IngestPipeline`'s inline embed on every bus event | Ingest (7.1), with embedding deferred | P1 |
| `giap-memory` and `giap-context` | One tool group (section 10) | P3 |
| Suggestion generation and the template suggestors | The Proposal agent | P5 |
| Proactive review and reminder proposals | The Proposal agent. Accepted reminders stay in `reminders` | P5 |
| Long-task artifacts | They stay as a task's working memory. A finished task's result and calendar are copied in as items | P5 |
| Quiet compaction | Stays: it works on goose's live conversation | n/a |

**Unwired code to delete in P1:**
- `memory_graph.rs`, and `memory_edges`, whose foreign key names a nonexistent `memories(id)`;
- `memory_graph_enabled` and `memory_extraction_first_pass_at`, which nothing reads;
- `AppState.mcp_memory`;
- `session_messages.embedding`;
- context retention's `purge_expired`, which nothing calls.

**Defects that go with the old systems** (verified 2026-10-28, and the files are unchanged on
`0309bf0d`):
- the `giap-memory` tools search at `ProfileScope::Household`, the open PAI-1 gap;
- MCP `save_memory` writes `profile_id: None`, making a member's own save household-visible;
- turn recall and `search_context` read the BLOBs on source rows, and only `recall` reads the index;
- the `SuggestionGeneration` job has no cancel watcher.

The first two are live privacy defects and are worth fixing before P3 (a task card offers it).

**Tests that read source as text**, rewritten to guard the new chokepoint and never deleted
without a replacement:
- `index_chokepoints_are_wired.rs`, `redaction_chokepoints_are_wired.rs`,
  `proactive_reviewer_is_wired.rs` and `background_writers_are_not_activity.rs`;
- `context_pipeline_is_wired.rs` and the two `context_producer_tracks_*` tests;
- the `<system-context>` envelope tests in `goose_agent.rs`.

---

## 4. The store

### 4.1 Items: one header, each kind's own details

Every item has one row in `items`, the header that every policy reads. What only one kind has stays
in that kind's own table, keyed by `item_id`. Today's tables become those detail tables: P1 gives
each an `item_id` and writes a header for every row, its text moves to the header, and its own
columns stay where they are.

| Column | Meaning |
|---|---|
| `id` | UUIDv7, so agent cursors are a single high-water mark |
| `kind` | `message`, `conversation`, `fact`, `entity`, `event`, `media`, `notification`, `mail`, `home_event`, `visit`, `report`, `suggestion`, `trait` |
| `source_id`, `external_id` | Unique together; ingest is idempotent on them |
| `owner_profile_id` | Required unless `audience = household` (a `CHECK`) |
| `audience` | `private` (default) or `household`; sharing with named members is a row in `item_shares` |
| `sensitivity` | `PrivacySensitivity` |
| `modality` | `text`, `image`, `audio`, `video` |
| `observed_at`, `valid_until` | When it happened, and when it stops mattering |
| `title`, `body` | Redacted text: for media, the OCR or caption text when there is any |
| `context` | One line that places the item: who, when, where it came from, and what it refers to (section 7.3). It is embedded and keyword-indexed with the body |
| `state` | `active`, `superseded`, `dismissed`, `accepted` |
| `produced_by` | `ingest`, or the recipe name and its definition hash, so a bad agent version's output can be found and withdrawn |
| `confidence` | For derived items |

`item_links(from_id, to_id, relation)` records `derived_from`, `evidence_for`, `mentions`,
`same_as` and `supersedes`.

| Kind | Details in | Holds |
|---|---|---|
| `fact`, `entity`, `trait` | `memory_fragments` | Segment, importance, `corrects`, kept, tier |
| `message`, `conversation` | `session_messages`, `sessions` | Role, turn and session fields. The rolling summary becomes the conversation's body |
| `mail`, `notification`, `event` from an account | `context_items` | Sender, thread, start and end, place: what the account sources fill today |
| `media` | `media_details` (new) | `blob_id` for the encrypted bytes, type, size, content hash |
| `home_event` | `home_event_details` (new) | Device, and the state before and after |
| `visit` | `visit_details` (new) | Place cluster, arrival and departure |
| `suggestion` | `suggestion_queue` | Status, decision, feedback |
| `report` | Long-task artifacts | Task and artifact |

A guard test fails the build when a kind names no detail table or no lifecycle.

### 4.2 Lifecycle per kind

The lifecycle is declared once, in `pond-core`.

| Kind | Written by | Default audience | Kept | When its source is deleted |
|---|---|---|---|---|
| `message`, `conversation` | chat, Conversation | private | Until the conversation is deleted | n/a |
| `fact`, `entity`, `trait` | agents, the model's own save | private | Until superseded | Deleted once its last `evidence_for` source is gone |
| `media`, `notification`, `mail`, `visit` | ingest | private | Per source setting | n/a: these are sources |
| `home_event` | ingest | household | 7 days, unless an agent cites it | n/a |
| `event`, `report` | ingest, agents, long tasks | the owner's | `valid_until` + 30 days; reports until deleted | Deleted when every source is gone |
| `suggestion` | Proposal | private | Until decided or `valid_until` | Withdrawn |

Approved actions do not cascade. An accepted reminder is the member's commitment.

### 4.3 The index

`pond_vectors.db` keeps its shape: `(item_id, model_id, modality, dims, vector)`, with no text.

- **Staleness.** An item without a vector for the current `model_id` needs one, so changing the
  model refills the index in idle time.
- **Vectors are personal data.** Inversion attacks such as Vec2Text reconstruct short inputs from
  their embeddings. Vectors sit on the encrypted volume, and retrieval joins them to live,
  scope-filtered items.

---

## 5. Embedding: EmbeddingGemma 2

EmbeddingGemma 2 (2026-10-06, Apache 2.0, built on Gemma 4) puts text, code, images, audio and video
in one 768-dimension space, with Matryoshka cuts to 512, 256 and 128, and an 8,192-token context.

- **Size.** Google's page gives 740M parameters in total and a 270M text model. News reports add a
  170M vision encoder and a 300M audio encoder, loadable separately.
- **Measured on the Orin by P0 (2026-10-31).** Section 5.1 has the figures;
  [the full record](../../scripts/jetson/embed-p0/RESULTS.md) sits beside the harness.

What it buys:

- **Pictures become searchable without the chat model's vision encoder.** The Orin still declares
  no picture support until an encoder is measured there (`DEVICE_MEASURED_VISION`).
- **One space for every modality.** A text question finds a photo, a voice note or a camera clip.
- **Over 100 languages.** A fact said in Swahili or Sheng is found by a question asked in English.

How it runs:

| Part | Loaded | On the turn path? |
|---|---|---|
| Text encoder | Resident, in MiniLM's place (about 140 MB net). If the budget refuses it, per batch, and the turn falls back to keyword recall | One query embed per turn: 18 ms on CUDA, 41 ms on the CPU, against a warm TTFT of 0.45 s |
| Vision encoder | Per batch, when free RAM covers its 290 MB | No |
| Audio encoder | Per batch, in quiet hours, when free RAM covers its 371 MB | No |

Free RAM moves with every helper snapshot and drafter change, so `device_budget.rs` gets one
constant per encoder, measured inside `pond-server`, before any of them ships. Use the model card's
task prompts word for word: `task: search result | query: ` for queries and
`title: {title or none} | text: ` for items.

### 5.1 What P0 measured

On the nano at MAXN_SUPER, beside E4B, its MTP drafter, the camera pipeline and the lane, with the
service never stopped. A Q8_0 GGUF converted from Google's weights, on upstream llama.cpp.

| Part | Memory beside the pond | Speed |
|---|---|---|
| Text encoder, CPU | 270 MB | Query 41 ms p50, 56 ms p95; 3 items per 250 ms batch |
| Text encoder, CUDA | About 300 MB in process (derived; 413-492 MB plus 264-400 MB of NvMap as a process of its own) | Query 18 ms p50; 24 items per 250 ms batch |
| Vision encoder | 290 MB on the CPU | 6.1 s an image on the CPU, 0.67 s on CUDA |
| Audio encoder | 371 MB on the CPU | 2.1 s for an 11 s clip on the CPU, 0.17 s on CUDA |
| MiniLM-L6, today's embedder | About 130 MB, standalone | 7 ms |

| Recall@5 | MiniLM-L6 | EmbeddingGemma 2 |
|---|---|---|
| Memory reachability, 70 facts | 0.95 (R@1 0.62) | 0.95 (R@1 0.76) |
| Personal context, 403 items | 0.80 | 0.84 |
| Swahili, Sheng and mixed, 69 items | 0.39 | 0.73 |
| All three in one index, 542 items | 0.53 | 0.74 |
| Text to image, 11 photos | n/a | 1.00 (R@1 0.91) |

What follows:

- **It replaces MiniLM; it never sits beside it.** The pond keeps 450-518 MB free while it serves.
  Beside MiniLM the text encoder would leave 180-250 MB, under the 300 MB margin; in MiniLM's place
  it leaves about 310-380 MB.
- **Vision and audio run per batch only.** Neither fits beside the serving pond.
- **Store 256 dimensions** unless the household's own store says otherwise. On these sets 256 equal
  or beat 768, at a third of the size. One query moves these small sets by 2 to 9 points.
- **The runtime is llama.cpp in process, through goose's llama-cpp-2**, with the text encoder on
  CUDA in the pond's own context and the same GGUF on the CPU when NvMap is scarcer. LiteRT-LM was
  4 times slower on the CPU, no lighter, runs fp16 on the GPU (which the card warns against) and
  does not batch. ONNX Runtime was not tried: Google publishes no ONNX and forbids fp16.
- **Wiring it, in P2:**
  1. Bump llama-cpp-rs-giap's llama.cpp (at `8ac8291`, v1 only) past upstream's `gemma-embedding2`
     support (ggml-org/llama.cpp#30054), its no-cache attention fix (#30416) and its pooled graphs
     without logits (#30255, #30445). The chat path shares it, so the bump needs the Orin
     regression run: E4B, the drafter and KV snapshots.
  2. Check the width against `n_embd_out` (768), not `n_embd` (512), which the GGUF provider
     compares today and would refuse.
  3. Keep one context: n_ctx 256-512, 16 sequences or fewer, unified KV.
  4. Map the token table on CUDA rather than pinning it, which kept 136 MB out of NvMap.
  5. Give it a `model_id` of its own, since nomic is also 768 wide.
- **Not measured:** the cost inside `pond-server`, recall on the household's real store, audio
  retrieval and video.

---

## 6. Counter-phase scheduling

### 6.1 Order

The lane already orders background work, and the context space slots into it:

1. a turn in flight, a voice session, or a held subagent permit: nothing else;
2. compaction;
3. a member's long task, one step at a time;
4. chores, including every context agent: the least recently run goes first, and one that has
   waited 15 minutes gets a turn;
5. the embedding slot (6.2).

Heavy agents (Consolidation, Profile, media backfill, re-embedding after a model change) also hold
to the household's quiet hours, as PAI-6 section 8.3 asks of long tasks.

### 6.2 The embedding slot

- **When it runs.** It is new, and it runs only when nothing in steps 1 to 3 is active. It may
  overlap a chore, because both are background work.
- **Batches.** Each batch must finish within a measured bound: 250 ms on the Orin for text, which
  P0 measured as 24 items on CUDA and 3 on the CPU. The slot checks the lane between batches.
- **Encoders.** The vision encoder loads only when free RAM covers its measured budget, and it
  unloads when serving starts.
- **No queue.** Work is found by the staleness join. Agents keep cursors
  (`agent_cursors(recipe, last_item_id)`), so a restart resumes where it stopped.
- **Deliberate shares go first.** A screenshot a member sends on purpose is embedded at the next idle
  moment, ahead of any backfill.

---

## 7. Agents

### 7.1 Ingest is a pipeline, not an agent

1. Receive from a route or connector, and resolve the principal (the paired device's member).
2. Normalise to an item, with an encrypted blob for media.
3. Redact, using the existing chokepoint, and drop one-time codes and 2FA notifications.
4. Deduplicate on `(source_id, external_id)`, and on the content hash for media.
5. Persist the item and its blob in one transaction.
6. Publish `BusEvent::Ingest` (PAI-8 P8; the variant does not exist yet).

### 7.2 Context agents are recipes

**Additions to the `giap_role` catalog** (`role_block.rs`), kept fixed and validated like the rest:

| Addition | Kind | What it does |
|---|---|---|
| `event: { on: context.ingest, kinds: [...] }` | Activation | Fires on `BusEvent::Ingest` for the named kinds. Its cooldown turns a burst into one run |
| `event: { on: session.quiet }` | Activation | Fires once a conversation has been quiet for its cooldown (`BusEvent::Session` exists; the activation does not) |
| `context` | Before hook | The owner-scoped items most relevant to the brief, within a token budget |
| `save_items` | After hook | Writes facts, entities, events and traits from the hand-back, each linked to its sources and stamped with the recipe's hash |
| `propose` | After hook | A suggestion to the owner's own devices, with its evidence. The decision comes back as feedback |

**The shipped agents.** Goose names are left to the existing naming.

| Agent | Ships as | Activation | Hooks | Replaces |
|---|---|---|---|---|
| Conversation | Default, compiled every start | `session.quiet` | `context` before, `save_items` after | Extraction, summary refresh, titling |
| Consolidation | Default | `schedule`, daily, in quiet hours | `context` before, `save_items` after | Consolidation |
| Understanding | Suggested | `context.ingest` for mail, media, notifications and visits | `context` before, `save_items` after | n/a (new) |
| Profile | Suggested | `schedule`, weekly, in quiet hours | `context` before, `save_items` after | n/a (new) |
| Proposal | Suggested | `context.ingest`, `presence.arrive`, `time.daily`, and camera and sensor events | `context`, `today` and `weather` before, `propose` after | Suggestion generation, proactive review |
| Planning | Exists: the composer role and deep research | An accepted suggestion, through `start_task` | n/a | n/a |

Conversation and Consolidation are default because they replace behaviour that is always on today.
The other three are suggested: nothing new runs until a member turns it on, as P19 already holds.

**Principal.** A context agent runs at the scope of the item or conversation that woke it, and an
agent woken by a household item reads only household items. This is the existing `member: owner`
rule, extended to agents the household owns.

**Untrusted content.** Mail, notifications, pages and text read off screenshots can carry
instructions aimed at the model. An agent that reads them:
1. holds no tool that sends, buys, moves or fetches, so `giap-knowledge` is withheld;
2. writes only items and proposals;
3. shows the member which items a proposal came from before they approve it.

Untrusted text reaches the model inside a data block, never as instructions.

**Cost.** Each default agent adds one compiled prefill (about 20-30 MB on disk) and one entry to the
boot compile pass, which already waits behind the household's turns.

### 7.3 Memory: contextual extraction and adversarial consolidation

Both build on code that exists. Batch extraction already shows the model up to eight known memories
(about 200 tokens), mines only conversations whose speaker is known, and puts every candidate
through a write gate in code, because a 4B model ignores parts of its prompt. Consolidation already
has a Proposer, an Adversary and a Judge (`three_stage_consolidator.rs`), off by default behind
`memory_consolidation_mode`.

**Contextual extraction** is how the Conversation agent writes, and Understanding for ingested
items:

1. **What the call sees.** The window (complete exchanges, up to 6,000 characters) and its context:
   - who is speaking, as the session resolves it, never from the text;
   - the date, and whether it was typed, spoken or sent from a phone;
   - the conversation so far, as its `conversation` item's body;
   - the items already known at the speaker's scope, from the unified retriever: facts, entities
     and events, within today's token budget.
2. **What it writes for each candidate:**
   - the fact, its subject and its kind;
   - how it relates to a shown item: new, an update to it, or a contradiction of it;
   - a context line saying who, when, and what "it" or "there" refers to.
3. **The context goes into the vector.** An item is embedded and keyword-indexed as its context line
   plus its body. On Anthropic's retrieval sets,
   [contextual embeddings](https://www.anthropic.com/news/contextual-retrieval) cut top-20 retrieval
   failures by 35%, and by 49% with a contextual keyword index. Here the line costs no extra call:
   the extraction call already holds the window.
4. **The write gate stays in code.** `fact_defect`, the subject gate and banding by vector run on
   every candidate. The owner comes from the principal, never from the model's output. Each fact
   links `evidence_for` to the items it came from.

A fact whose only evidence is untrusted (mail, a notification, a page, text read off a screenshot)
is visible as such from its sources' kinds. Turns see it labelled, and Profile and Proposal never
build on it until a consolidation run has passed it.

**Adversarial consolidation** makes the three stages the Consolidation agent's default:

1. **Proposer.** Merges, splits, supersedes, prunes and recategorises within a bounded batch, as
   today, now seeing each fact's context line and its sources.
2. **Adversary.** Challenges every proposal. Today it guards against lost information and against a
   correction being overridden. It gains three challenges:
   - *unsupported*: the result says more than its sources do, and it is shown those sources;
   - *scope*: it would widen a member's private fact to the household, or merge two members' facts;
   - *planted*: its only evidence is untrusted content that tells the assistant how to behave, or
     makes a lasting claim about a member. Memory injection works: in
     [MINJA](https://arxiv.org/abs/2503.03704), ordinary queries alone planted records in an
     agent's memory 98.2% of the time.
3. **Judge.** Decides each disputed proposal, as today.
4. **Code has the last word.** `apply_actions` stays the only write path. It already protects
   corrections and kept items. It adds that scope never widens and that a merge keeps every
   source's evidence. Nothing is deleted: a replaced item becomes `superseded` with a link, so a
   bad run can be withdrawn by its recipe hash.

The *planted* challenge is the model's judgement plus provenance read from the item's sources,
never a keyword filter.

**Cost.** Three calls a batch instead of one, as a chore in quiet hours, so it spends idle time and
never turn time. P3 measures calls, seconds and accepted changes per run on the Orin.
`memory_consolidation_mode = single` stays selectable for a pond whose quiet hours are too short.

**Tested by P3's gate:**
- contextualised recall beats plain recall on the memory-reachability fixture, on the Orin;
- a planted set (instructions and false facts about members, hidden in mail and pages) leaves no
  unflagged fact that a turn or an agent would act on.

### 7.4 Four flows

1. **Screenshot to outing.**
   1. A member shares a screenshot of a restaurant. The phone sends it with its OCR text.
   2. Understanding writes a `place` entity linked to the image.
   3. On Friday evening Proposal sees the place, a free Saturday and the weather, and asks the
      member's phone "Lunch at the place you saved on Tuesday?".
   4. Accepting starts a planning task. The composer writes the plan and a calendar, and both
      become items.

   A place lookup on the web carries the place's name only, and only with the nearby-places switch
   on.
2. **Mail to reminder.** IMAP delivers a school email. Understanding writes the `event`, and
   Proposal offers a reminder to both parents, because the event is a household item.
3. **Home routine.** `home_event` items show the porch light switched off by hand at about 23:00 on
   twelve of fourteen nights. Proposal offers an automation. Nothing changes until a member approves
   it.
4. **Recall.** "Mama ana mzio wa karanga", said in a conversation, becomes a fact with that
   conversation as its evidence. A week later "what is mum allergic to?" finds it.

### 7.5 Internet sources

Each connector runs on the pond and opens its own connection outward through the egress gate, so
nothing on the pond listens on the internet. Each is a source with its own switch and a member's
consent. What it brings in goes through the ingest pipeline as untrusted content (section 7.2).

| Source | Connector | How it arrives |
|---|---|---|
| Mail, any provider | `pond-adapters-imap` with IMAP IDLE (RFC 2177, in `async-imap`), one connection per mailbox | Pushed while the connection holds. A sweep on reconnect catches what a dropped connection missed. Today it is a 30-minute poll |
| Mail, JMAP providers (Fastmail, Stalwart) | JMAP push over a WebSocket (RFC 8887) | Pushed. The state token resumes a sync after a gap |
| Calendars | `pond-adapters-caldav`, as now | Its timer, made incremental by the collection's sync token |
| Home Assistant | The pond as a client of Home Assistant's own MCP server (`/api/mcp`, since 2025.2), signed in with its OAuth (PKCE) | Its tools and its live-context snapshot, covering only the entities the household exposed to it. State changes become `home_event` items, as in P6 |
| Messages | Matrix (the client-server `/sync`), and a Telegram bot (`getUpdates`) | Long-held requests. A Telegram bot sees only what is sent to it |
| Webhooks | Signed, over the tailnet only | Pushed |

A connector's tools that send or change anything are actions, and a member approves each one
(rule 3).

**Left out:**

| Option | Why |
|---|---|
| Hosted connector platforms (Composio, Arcade, Pipedream, Zapier, Nango) | Their servers hold the household's tokens and see its content (rule 4) |
| Personal WhatsApp bridges | They speak WhatsApp's private protocol, against its terms |
| Signal | No official interface for a personal account |
| Gmail push (`watch` with Pub/Sub) | Each household would need its own Google Cloud project |
| Microsoft Graph change notifications | They need a public HTTPS address. Graph's delta queries on a timer do the same job |

The marketplace keeps its bundled servers. It can later search the official MCP registry; a server
found there runs only after a member installs it, behind the same egress gate.

---

## 8. From the phone to the pond

The phone's proposal,
[`docs/PHONE-CONTEXT.md`](https://github.com/jarida-io/goose-on-the-go/blob/main/docs/PHONE-CONTEXT.md),
is kept: collectors, a reducer, an outbox, and sync over a proven route only. None of it is built
yet.

### 8.1 What exists and what doesn't

| Exists | Missing |
|---|---|
| CPace pairing, SPKI-pinned HTTPS on :4443, the embedded tailnet node | `POST /api/v1/context/ingest`; `SourceKind::Mobile` is still `AwaitingIngestRoute` |
| Photos picked in chat, kept as attachments | Media items: `ItemKind` has no media kind, and there is no blob store |
| FCM wakes carrying only `notification_id`, `category` and `wake` | APNs. The phone appears not to act on a wake until it is opened |
| Agent, My Pond and queue screens | Collectors, a share target, a persistent outbox, background work, resumable upload |

### 8.2 Sources

| Source | Android | iOS | Mode |
|---|---|---|---|
| Deliberate shares (screenshots, photos, links, text) | Share intent | Share extension | Eager |
| Screenshot and photo albums, per album | MediaStore | PhotoKit, limited library | Lazy: on Wi-Fi, while charging |
| Message and mail notifications, per app | `NotificationListenerService` | Not available to apps | Eager, text only |
| Calendar, contacts | Read on the phone | Read on the phone | Changes only |
| Places | Visits, clustered on the phone | `CLVisit` | Visits, never a raw trail |

The phone does the cheap perception: OCR (ML Kit, Apple Vision), clustering places, thumbnails,
content hashes, and dropping one-time codes.

### 8.3 Transport

1. **Channel.** Pinned TLS to the pond, inside the tailnet's WireGuard when the phone is away.
   Nothing between the phone and its pond terminates it, so it is end to end. P1 seals the pond's
   TLS key to the fTPM (section 9). A separate payload envelope (HPKE) is deferred until something
   stores and forwards, such as a relay while the pond is down.
2. **Outbox.** It is encrypted on the phone under a key held behind Face ID or a fingerprint, as
   #715 holds the admin key, and keyed `(source, external_id)`. Media goes up in resumable chunks.
   The phone deletes its copy once the pond acknowledges a durable write.
3. **Identity.** The member comes from the paired device. That needs `Principal.proven_profile_id`
   filled in the auth middleware. It is still never set outside tests on `0309bf0d`, and this is
   P4's prerequisite.
4. **Back to the phone.** A suggestion travels as a wake with no content, and the phone fetches it
   over its proven route. iOS needs APNs and the paid Apple developer team.

---

## 9. Keeping it secret

### 9.1 What is true now

- The three databases are plain SQLite. Snapshots, face embeddings and the TLS key are plaintext
  files.
- `secrets.json` is encrypted, under a key file kept beside it.
- `agamemnon` keeps the pond's data in `/var/lib/goose-in-a-pond` (`0750`), away from the login.
- The nano has an OP-TEE firmware TPM with `tpm2-tools`. When the admin card is made, its recovery
  copy is sealed to it under the owner hierarchy, so that copy cannot be opened from the disk on
  another board.
  PAI-2 section 3.4 says an Orin Nano devkit exposes no fTPM; that is wrong, and this change
  corrects it there.

### 9.2 Key hierarchy

| Layer | Protects | Key |
|---|---|---|
| Data volume | `/var/lib/goose-in-a-pond` | LUKS2. One keyslot is sealed to the fTPM. One opens with the admin card, so the printed card is the household's recovery key. Whether the nano's systemd can unlock a TPM2 keyslot at boot is a P1 measurement |
| TLS identity, secrets key | The phone channel, connector tokens | Sealed to the fTPM. At service start, the root helper that already seals the admin card unseals them for `agamemnon` |
| Blob | Each media item | A random key per blob, wrapped by the volume key. Deleting the item destroys the key |
| Database | Every row | SQLCipher, only if its measured per-turn cost is small. The volume carries it otherwise |
| Mac | The same layers | The login Keychain holds the admin card already. FileVault covers the disk |

### 9.3 What each layer stops

| Threat | Stopped? |
|---|---|
| A copy of the disk, or storage pulled from the board | Yes, from P1: the volume key never leaves the fTPM |
| The whole board taken, on an unfused board | No. Without secure boot, a modified OS can run as root and unseal. Fusing OemK1 closes this (decision 2) |
| The network | Pinned TLS, and WireGuard off the LAN |
| Another member | The principal filter (rule 2) |
| A guest | Sees nothing |

The pond holds no per-member keys. Every key lives on one appliance, so per-member keys would add
work and no protection.

### 9.4 What the household controls

- One switch per source and per member. Nothing collects until it is on.
- An activity view of what was indexed and when.
- "What the pond knows about me": facts and traits with their evidence, each deletable.
- Disconnecting a source deletes its items and says how many.
- An export of everything a member owns.

---

## 10. Surfaces

**For the model.** One tool group replaces `giap-memory` and `giap-context`:
- `search_context(query, kinds?, since?)`, whose results can be pictures;
- `get_item(id)`;
- `remember(fact)`.

`search_context` joins `HOUSEHOLD_READ_TOOLS`. The tool block changes once at upgrade, and the boot
compile pass rebuilds the affected prefills then.

**For the household** (DESIGN.md governs every screen):

- **Search.** One box, results of every kind, pictures inline, each saying where it came from.
- **Suggestions.** Each card shows its reason and evidence, with *Add it*, *Not now* and *Not like
  this*. The last two teach Profile and Proposal.
- **Agents.** The context agents appear on the existing Recipes screens on both UIs, with their
  prefill state, and the suggested ones as cards.
- **Sources** and **What the pond knows**, beside them.
- **In My Pond:** the index's health and its size on disk.
- **On the phone:** "Send to pond" in the share sheet, suggestions as notifications, and decisions
  from the notification itself.
- **By voice:** "anything for me today?" reads the member's open suggestions.

---

## 11. Phases

Each phase is stamped `LANDED` and then `VERIFIED` on the Orin, and deletes what it replaces in the
same release. Before any restart on the nano, check that no long task is running.

| Phase | Builds | Deletes | Gate |
|---|---|---|---|
| P0 Measure | EmbeddingGemma 2 on the Orin: RAM per encoder beside E4B and its drafter; query-embed latency against the warm TTFT; recall against MiniLM on the two corpora, a Swahili and mixed-language set, and image queries. Pick the runtime | Nothing | It fits the budget and beats MiniLM. **Measured 2026-10-31:** it passes for the text encoder in MiniLM's place, on llama.cpp; vision and audio run per batch (5.1) |
| P1 Store and keys | The item header and links, with a header for every memory, summary, context item and suggestion, each table kept as that kind's details; the lifecycle per kind. The LUKS2 volume on the fTPM with the admin card as recovery; TLS and secrets keys sealed; per-blob keys | The `Corpus` split, the unwired code, and the old tables a release later | The nano's real store migrated, then re-run; the volume opens at boot and not from a disk copy |
| P2 Embedding slot | The slot in the lane's order; the text encoder, wired as 5.1 lists; re-embedding | Startup backfill, `IndexMaintenance`, and MiniLM in the same release, since the budget has no room for both | No turn or long-task step measurably slower while it runs |
| P3 Recipe catalog and default agents | `context.ingest` and `session.quiet` activations; `context`, `save_items` and `propose` hooks; Conversation and Consolidation; the unified retriever and `search_context`; contextual extraction and adversarial consolidation (7.3) | Extraction, summary refresh, titling, consolidation, `topical_memories`, `giap-memory`, `giap-context` | pai-bench PAI-3 and PAI-8 on the Orin; one activation fired there (P16 has none yet); the two memory tests in 7.3 |
| P4 Phone | The ingest route, the share target, the outbox, wakes and APNs. Then OCR, visits, notifications and albums, each behind its switch | The chat-only photo path, as an ingest case | A shared screenshot is found by a text search on the Orin |
| P5 Suggested agents | Understanding, Profile and Proposal; long-task results as items; feedback | Suggestion generation, proactive review | A real suggestion reaches a phone and is decided there |
| P6 Multimodal and home | Vision and audio encoders; `home_event` items from cameras, sensors and Matter | n/a | Encoders within the measured budget |
| P7 Internet | The connectors in section 7.5: IMAP IDLE in place of 30-minute polling, JMAP push, CalDAV, Home Assistant's MCP server, Matrix and a Telegram bot, and signed webhooks over the tailnet only | The mail poll | A new email becomes a suggestion with no public port open |
| P8 Fused boards | Secure boot and the EKB on shipped units | n/a | Decision 2 |

**What has to land first:**
- `proven_profile_id` (PAI-2), before P4;
- principal-scoped `get_session_messages` (PAI-1), before P3;
- roles (#707) and `HOUSEHOLD_READ_TOOLS` (#717), before P3, because the audience rules and
  `search_context` build on them.

---

## 12. Decisions owed

| # | Decision | Recommendation |
|---|---|---|
| 1 | Storage: one item header over each kind's own detail table, rather than one table holding everything | Chosen 2026-10-31 |
| 2 | Fuse OemK1 on shipped ponds: irreversible, and it alone protects a whole stolen board | Yes for shipped units, after a rehearsal on a spare board. No longer a prerequisite: the fTPM covers a copied disk now |
| 3 | SQLCipher on top of the encrypted volume | Decide from P0's per-turn cost |
| 4 | Mail: subjects or bodies | Subjects, senders and dates first |
| 5 | Guests and household items | Guests see nothing |
| 6 | The admin card as the data volume's recovery key | Yes. Rotating the card re-wraps that keyslot |
| 7 | The paid Apple developer team, for iOS wakes | Needed before iOS syncs while the app is closed |
| 8 | Conversation and Consolidation on by default; Understanding, Profile and Proposal suggested | Yes, as P19 already handles templates |
| 9 | Mail sign-in beyond app passwords: OAuth for Gmail and Outlook | App passwords first. Read Google's and Microsoft's terms for mail scopes before building OAuth |
| 10 | Adversarial consolidation by default: three calls a batch | Yes, once P3 shows it fits the Orin's quiet hours |
| 11 | Where a pond downloads EmbeddingGemma 2's GGUF. Google ships safetensors only, and converting them needs torch, which cannot run beside the pond | `ggml-org/embeddinggemma-2-GGUF` at a pinned revision and sha256, once its vectors match P0's own conversion: the chat model already comes from `unsloth`. The alternatives are a GGUF we convert and host ourselves, or Google's LiteRT file on the slower runtime |

---

## 13. PAI check (checklist section 2.2)

| Question | Answer |
|---|---|
| Does it move the KV prefix? | Once, at upgrade. Per-turn context stays in `<system-context>` |
| Does it add preamble tokens? | No. One tool group replaces two. Each default agent has its own compiled prefill |
| Can `profile_id` be `None`? | Only for household items, enforced by a `CHECK` |
| New egress without the gate? | No. Every connector in section 7.5 goes through the gate, webhooks arrive over the tailnet, and push carries no content |
| A secret on `Settings`? | No. Keys are sealed to the fTPM |
| Can a guest reach personal data? | No |
| Does a turn block on an LLM call? | No. Only a query embed sits on the turn path, and it is measured |
| A side effect without approval? | No. Agents write items and proposals only |
