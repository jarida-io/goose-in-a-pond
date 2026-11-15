# PAI-8 — Personal context streaming

Requirement: *personal context streaming — data collected on-pond, on-mobile, and from internet
accounts (Google, OneDrive, iCloud, Slack, Telegram, WhatsApp, e-mail).* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: [PAI-1](./01-identity-and-profile-boundaries.md) (whose data),
[PAI-2](./02-privacy-and-security-guardrails.md) (guardrails), [PAI-7](./07-proactive-intelligence.md)
(what makes it useful).

Verified against code 2026-08-03.

> Status (2026-10-23): P1 and P5 are landed (P5: read-only IMAP and CalDAV,
> 2026-08-17/18); P2 is partial (tools only, no `<system-context>` path); P3, P4, P6, P7 and P8 are
> not started. Section 4 carries the per-phase detail. Programme summary:
> [checklist section 0](./00-checklist.md).

---

## 0. Posture

**Ingest-only, opt-in per account.** Data flows into the pond and does not flow back out. Inference
stays entirely local; no prompt, no memory and no conversation is ever sent to a connected account.
A connector's network access is limited to the account's own API, and every request is
`record_egress`-tracked and governed by `network_mode` (PAI-2).

Writing back — sending an e-mail, posting to Slack — is **out of scope for v1** and, when it is
considered, goes through the draft-approval gate per PAI-2 §3.6.

---

## 1. What is true today

### 1.1 No personal-account integration exists

Grep across `crates/` (excluding the submodule and `vendor/`) for Gmail, Google Calendar, Drive,
OneDrive, iCloud, Telegram, WhatsApp, IMAP, SMTP and RSS returns **nothing**. The only `google`
matches are the FCM push relay and two unrelated comments.

> No longer true since 2026-08-17: P5 added read-only IMAP and CalDAV sources (section 4).

### 1.2 But most of the machinery is already here

**OAuth 2.1 with PKCE — complete, and used once.** `pond-api/src/oauth_callback.rs` implements the
full flow: in-memory `PkceSession` keyed by a CSRF state nonce, S256 challenge (`:102-112`),
`FlowOutcome`/`OAuthOutcomes` with a 300 s TTL, and `GIAP_INTERNAL_TOKEN` / `GIAP_SERVER_URL` handed
to spawned extension subprocesses (`:120-142`). Routes at `routes.rs:216-220`.
`builtin_oauth_providers()` (`user_data/services/oauth_providers.rs:11-39`) returns **exactly one
provider: Spotify** — with a test asserting `providers.len() == 1`.

**An extension marketplace.** `pond-core/src/extensions/marketplace_registry.json`, seven curated
npx-spawned MCP servers — filesystem, github, brave-search, memory, puppeteer, **slack**, music.
Installable via `routes.rs:225-230`, with repo-relative script args anchored at construction
(`mcp/services/marketplace.rs:34-58`).

**A template for well-behaved outbound adapters.** `pond-adapters-weather` owns its own
`reqwest::Client` with explicit timeouts and wraps every request in `traced_send`, which calls
`record_egress` (`lib.rs:15-30`). Base URLs are consts with a `with_base_url()` used only by wiremock
tests.

**Secret storage.** `SecretRepository` (`security/ports/secret.rs`, the trait and nothing else) with
`SecretKind::{ApiKey, OAuthFlow, Generic}` — which lives in `security/domain/secret.rs`, a different
module. Re-verified 2026-08-09; the citation used to name one file for both and there is no enum in
the port file. Values are never returned through REST.

### 1.3 What is missing structurally

- **No inbound ingestion of any kind.** Webhooks are outbound only; there is no receiver. Grep for
  an `/ingest` or `/context/` route returns nothing — still true, re-verified 2026-08-09. **But the
  citation was pointing at dead code**, which matters because P7 will want to wire its receiver
  beside the sender: `pond-infra-scheduler`'s `WebhookTaskExecutor` is re-exported by that crate's
  `lib.rs` and constructed nowhere, and has been dead since PAI-2 P3 found it on 2026-08-05. The
  live outbound path is the `TaskKind::Webhook` arm in `pond-server/src/schedule_executors.rs`,
  which carries its own comment saying there are two webhook executors and they are not
  interchangeable.
- **GOTG pushes almost nothing.** The mobile companion has no dedicated crate and no dedicated
  endpoints — it drives the same REST API with `client_type: "gotg"`. It sends exactly two things:
  its push token (`routes.rs:140-143`) and a heartbeat. No contacts, location, calendar, photos or
  health data. **Correction, 2026-08-09 — this sentence used to say the only upload paths in the
  whole API are `/transcribe`, `/faces/*` and chat image attachments, and that was already known to
  be false**: `00-checklist.md` recorded the undercount on 2026-08-04 and the correction was never
  propagated back here. Grepping `Multipart` in `routes.rs` today finds `transcribe`,
  `calibrate_wake_word`, `read_face_multipart`, `register_face_handler` and `identify_face_handler`.
  It matters because PAI-1 and PAI-2 both lean on that absence argument, and an absence argument
  that undercounts its own surface is worth nothing. Grep the symbol before you reuse the claim.
- **Memory has no notion of provenance beyond `source`**, and no per-item retention.

---

## 2. The gap

GIAP knows what you told it and what its sensors saw. It does not know that your flight moved, that
the landlord replied, or that the meeting you are about to miss exists. That is the difference
between an assistant and a very good chat interface — and it is the requirement that most needs the
first seven workstreams to have landed first.

---

## 3. Design

### 3.1 `ContextSource` and `ContextItem`

New module `pond-core/src/context/`:

```rust
pub struct ContextSource {
    pub id: String,
    pub kind: SourceKind,          // Sensor | Camera | Voice | Mobile | Mail | Calendar |
                                   // Files | Chat
    pub provider: String,          // "google", "microsoft", "slack", "gotg", "imap", …
    pub profile_id: String,        // PAI-1: never optional. A source belongs to a person.
    pub scopes: Vec<String>,       // what the user actually granted
    pub cursor: Option<String>,    // incremental sync position
    pub last_sync: Option<DateTime<Utc>>,
    pub status: SourceStatus,      // Connected | NeedsReauth | Error | Paused
}

pub struct ContextItem {
    pub id: String,
    pub source_id: String,
    pub external_id: String,       // for idempotent re-sync
    pub profile_id: String,
    pub occurred_at: DateTime<Utc>,
    pub kind: ItemKind,            // Message | Event | Document | Location | Task
    pub title: String,
    pub body: String,
    pub participants: Vec<String>,
    pub sensitivity: PrivacySensitivity,
}
```

`profile_id` is non-optional by construction. The single largest lesson from this exploration is that
`Option<String>` for an owner becomes `None` everywhere (see PAI-1 §1.2); this type does not offer
that option.

Migration `0041_context_sources.sql` and `0042_context_items.sql`. **Re-verified 2026-08-09, and the
numbers moved:** this document was written when 0039 and 0040 were free, and PAI-5 has since taken
both — `0039_reasoning_tokens.sql` (P2) and `0040_session_thinking.sql` (P6, `bc3aa9df`). Confirm the
next free number by listing `crates/pond-infra/migrations/system/` rather than trusting this line,
which will rot again. And whatever the number, the migration must work against a database that
ALREADY HAS ROWS: every install after the first is an upgrade, and a migration that only works on an
empty file works exactly once. Retention is governed by
`retention_events_by_category` and `retention_sensitive_days` — both of which already exist and are
currently **headless** (`settings.rs` `HEADLESS_BY_DESIGN`). This workstream gives them a UI, because
"how long does GIAP keep my e-mail" is not a setting to hide.

### 3.2 The pipeline

```
source adapter  →  ContextItem  →  redact (PAI-2)  →  classify sensitivity  →
      →  persist (context_items, retention-governed)
      →  embed (fastembed, the same MiniLM used for memory)
      →  publish BusEvent::Ingest  (PAI-7's proposer wakes)
```

Items are **not** written into `memories` wholesale. Memory is a curated store with decay,
consolidation and importance; a mailbox would drown it. Instead the retrieval path gains
`context_items` as a second vector-searchable corpus, retrieved with the same
`memory_relevance.rs` blend of semantic similarity, recency and importance, and injected into
`<system-context>` under its own budget line so it can be cut independently.

### 3.3 Connectors, ordered by whether an honest implementation exists

| Source | Route | Reality |
|---|---|---|
| **Google** (Gmail, Calendar, Drive) | Extend `builtin_oauth_providers()` | Clean OAuth + REST APIs. The reference implementation. |
| **Microsoft** (Outlook, OneDrive) | Same | Graph API, comparable quality. |
| **IMAP e-mail** | New adapter, app password in `SecretRepository` | Works with anything, including self-hosted. Often the best privacy answer. |
| **Slack** | Existing marketplace entry | Already registered; needs token wiring and an ingest bridge. |
| **CalDAV / CardDAV** | New adapter | Covers Fastmail, Nextcloud, and iCloud calendars in practice. |
| **iCloud** | CalDAV + app-specific passwords | **There is no general iCloud API.** Mail via IMAP, calendar via CalDAV, and that is the honest ceiling. |
| **Telegram** | Bot API, or a user-run bridge | The Bot API only sees what is sent to the bot. Full account access needs MTProto, which is a user's own decision to run. |
| **WhatsApp** | Business API, or a user-run bridge | **There is no sane official read API for a personal account.** GIAP will not ship an unofficial client. If a user runs a bridge, GIAP ingests from it like any other source. |

Stating the last two plainly is part of the design. A roadmap that lists "WhatsApp" beside "Gmail"
as though they were comparable engineering promises something that cannot be delivered without
either a business account or a terms-of-service violation.

**On-mobile (GOTG)** is a first-class source, not an afterthought: calendar, location and (opt-in)
contacts pushed to `POST /api/v1/context/ingest`. The phone is where most personal context actually
lives, and GOTG already has an authenticated, rate-limited, paired channel.

### 3.4 New surfaces

- `POST /api/v1/context/ingest` — authenticated, for GOTG and any local pusher. Idempotent on
  `(source_id, external_id)`.
- `POST /api/v1/context/webhook/{source_id}` — for push-capable providers, with per-source secret
  verification. The first inbound webhook receiver in the product.
- `GET|POST|DELETE /api/v1/context/sources` — connect, list, disconnect.
- A `giap-context` MCP extension: `search_context`, `get_recent_context`. Read-only, joining the
  tool-group catalog so [PAI-3](./03-context-governor.md)'s narrowing applies.

### 3.5 Sync

Pull-only, cursor-based, scheduled through the existing scheduler with exponential backoff and
`NeedsReauth` surfaced in the UI rather than retried into a rate limit. No realtime sockets in v1 —
a home server that holds long-lived connections to five providers is a reliability problem before it
is a feature.

Each connector owns its `reqwest::Client` and wraps requests in `traced_send`, per the weather
adapter.

### 3.6 The hard rules

1. Every `ContextItem` carries a `profile_id` and a `PrivacySensitivity`.
2. A `Guest` session sees no context items. None.
3. Redaction runs before persistence, not before retrieval — the store never holds what it does not
   need.
4. Connector tokens live in the encrypted secret store (PAI-2 §3.4), never in `Settings`.
5. `network_mode` governs which hosts a connector may reach. In `offline`, connectors report
   `Paused`, not `Error`.
6. Disconnecting a source deletes its items by default, and says how many.
7. No prompt, memory or conversation content is ever transmitted to a connected account.

---

## 4. Phases

- **P1** Domain + migrations + the ingest pipeline with **on-pond sources only** (sensor, camera,
  voice transcripts) — proves the pipeline with data GIAP already holds and no new egress.
  **LANDED 2026-08-11 and REACHED.** The domain, migrations and adapter landed first
  (`b58361e1`, `a9062615`, `468e33d8`) with nothing that could produce a `RawItem`; the producer,
  the wiring and the connect surface followed the same day. `BusProducer` bridges the event bus to
  the pipeline, `main.rs` builds the repository and a `BusIngest` subscriber, and
  `POST /api/v1/context/sources` is what lets a source exist at all -- without it `upsert_source`
  had no caller and the corpus was empty by construction.
- **P2** Retrieval: `context_items` as a second corpus in `<system-context>` with its own budget;
  `giap-context` MCP extension. **PARTIAL (2026-10-23 code check): the extension LANDED 2026-08-11;
  the `<system-context>` corpus did not.** Context items reach the model only through the read-only
  `giap-context` tools (`search_context`, `recall`, `get_recent_context` in
  `crates/pond-mcp-server/src/context.rs`), registered behind `ext_context_enabled`, which ships off
  (on since 2026-11-14, with a passive `<personal-context>` block; see
  [personal-context-index.md](../personal-context-index.md)).
  No prompt path reads `context_items`; whether one earns its tokens is the measured passive tier in
  [personal-context-index.md](../personal-context-index.md), phase G. There is no
  `ingest_context` tool and there will not be one: the corpus is written by the pipeline from
  sources the household connected, and a tool that let the model write into it would let a prompt
  injection plant a memo the assistant later quotes as fact.

#### What landed, what did not, and why the order was forced

The storage half shipped a day before anything could reach it, and for that day the checklist said
`DESIGNED` while the master roadmap repeated it -- which produced a documentation error, because
`pub mod context;` makes all of this compile and a `pub` item in a library crate never earns a
`dead_code` warning, so nothing complains. `context_pipeline_is_not_wired_yet.rs` was written to
assert the absence and it caught the wiring the same day; its successor,
`crates/pond-core/tests/context_pipeline_is_wired.rs`, now asserts the presence.

**What is now live:** `BusProducer` and `BusIngest` (P1's producer), the repository and pipeline in
`main.rs`, `init_context_deps`, and `POST|GET|DELETE /api/v1/context/sources`. A member can connect
their front-door camera or a motion sensor; its events become redacted, sensitivity-classified,
retention-governed rows owned by them, and no other member's scope can read them.

**`giap-context` is registered, behind `ext_context_enabled`, which ships OFF -- and the toggle is
what resolves the objection to registering it at all.** Two read tools in every turn's prompt is a
real cost where tool schemas are already ~88% of a 4 096-token window, and until somebody connects a
source they could only answer "nothing found". Off by default means a pond that has not connected
anything pays nothing, and a household that has can switch it on. It is the SECOND extension toggle
to ship off, after `giap-orchestrator`, and
`only_the_deliberate_extension_toggles_ship_switched_off` was renamed from
`exactly_one_...` when it fired -- which is the deliberate decision that guard exists to force.

The count moved in the three places `registration_matches_the_catalog.rs` ties together: 16 -> 17
extensions, 62 -> 64 tools. `giap-context` is also on `groups_denied_to_guests`, alongside
`giap-memory` and for the same reason -- invariant 2 is "a Guest sees no context items. None.", the
tool layer already enforces it by scope, and this stops the tool being OFFERED, which on a small
model is the difference between a refusal and a turn spent discovering one. It is deliberately NOT
on `groups_denied_to_subagents`: it is read-only and a child inherits its parent's scope, exactly as
`giap-memory` does.

**One thing worth stating about the owner**, because it is the phase's security decision.
`ContextSource.profile_id` is not an `Option`, so a source belongs to one member, and the connect
route has **no `profile_id` field in its request body**. The owner comes from `resolve_turn_scope`
-- PAI-1's whole lattice, including the paired-device rung that landed the same day. A body-supplied
owner would be the hole PAI-1 P4 closed on `PUT /sessions/{id}/user`, and worse here, because every
item the source ever produces inherits it and migration 0044 refuses to let it change afterwards.
`Household` and `Guest` are refused rather than defaulted: a guest who could create a source would
be writing into a member's corpus.

#### The producer needs a source, and nothing can make one — the ordering this implies

Verified 2026-08-11 by reading the port rather than the phase list. `ContextRepository::upsert_source`
exists and has **no caller**, and `ContextSource` requires a `profile_id` that is not an `Option`,
because "an `Option` owner becomes `None` everywhere" is PAI-1 §1.2's lesson and this type refuses to
repeat it. So a sensor, camera or voice source belongs to **one household member**, deliberately, and
P1's phrase "the ingest pipeline with on-pond sources only" quietly presupposes that a source can be
created. Nothing can create one: `POST /api/v1/context/sources` is in section 3.4 and is **P3's**.

That is not a defect in either phase, it is an edge the phase list does not draw, and it decides the
order of the four steps above. Two consequences worth writing down before somebody starts:

- **Do not register `giap-context` before there is a producer.** `search_context` and
  `get_recent_context` would join the per-turn tool set and cost prompt tokens on every turn, for a
  corpus guaranteed to be empty on every pond. The Orin measurement makes this concrete: 61 tools
  are already ~25 000 characters of schema against a 4 096-token window, and tool schemas are ~88%
  of the preamble. Adding two tools that cannot return anything is a straight loss, and PAI-3's
  asymmetry rule — preamble is re-prefilled, working set is not — is the reason it is a loss on
  every turn rather than once.
- **Deciding which on-pond data becomes context is a design question, not a wiring one.** Two
  candidates look easy and are not. Voice transcripts would pour every conversation turn into the
  corpus, which is the mirror image of the mistake section 3.2 exists to prevent — memory is not
  allowed to be a mailbox, and the corpus should not be allowed to be a chat log; memory extraction
  already curates from those turns. Sensor readings have no owner: a hallway thermometer is a
  household fact, and this type cannot express one, so somebody has to say whose sensor it is. The
  coherent answer is probably per-member sources a person connects on purpose — a sensor in *my*
  room is *my* context — which is the same answer as "wait for P3's surface".

**One thing this unblocks that the ledger had wrong.** PAI-2 P6b's third part — P3's redaction
chokepoint — was recorded as circularly blocked on PAI-8. It is not: `IngestPipeline::new` takes a
`Redactor` that is deliberately **not** an `Option`, so the chokepoint exists in the type as soon as
anything constructs a pipeline. The dependency runs one way.
- **P3** `POST /context/ingest` + GOTG calendar and location. **Not started:** `SourceKind::Mobile`
  is still `AwaitingIngestRoute`. `fixtures/personal-context/household.jsonl` is written in the
  payload shape the route would take.
- **P4** Google connector as the OAuth reference implementation, extending
  `builtin_oauth_providers()`. **Not started.** Gmail works today through P5's IMAP with an app
  password; Google Calendar does not, because its CalDAV requires OAuth
  (`pond-desktop/src/connections/providers.ts`).
- **P5** IMAP and CalDAV — the self-hosted and iCloud path. **LANDED 2026-08-17/18** (never stamped
  here until 2026-10-23). `pond-adapters-caldav` reads a calendar with a date-range `calendar-query`
  and nothing else; `pond-adapters-imap` reads subjects, senders and bodies with `BODY.PEEK` (so
  nothing is marked read), over implicit TLS on 993 only. `pond-server/src/account_sync.rs` syncs
  connected `Calendar` and `Mail` sources into the pipeline on a schedule (`POST /context/sync`
  asks now), skips an unchanged calendar by its ctag, pauses when the pond is offline and marks a
  401 `NeedsReauth`. Both kinds are `SourceAvailability::Landed`. Setup:
  [`docs/guides/connect-calendar-and-mail.html`](../../guides/connect-calendar-and-mail.html). No
  hardware run of either connector is recorded in this programme.
- **P6** Microsoft Graph; Slack ingest bridge over the existing marketplace entry. **Not started**
  (`Files` and `Chat` are `AwaitingReadConnector`).
- **P7** Webhook receiver with per-source secret verification. **Not started.**
- **P8** `BusEvent::Ingest` wired to PAI-7's proposer — the phase where this stops being a database
  and starts being an assistant. **Not started:** `BusEvent` has no `Ingest` variant.
- **DEFERRED** Telegram and WhatsApp bridges; write-back of any kind.

**A retrieval surface spanning this corpus, memories and conversation summaries is designed in
[`docs/architecture/personal-context-index.md`](../personal-context-index.md)** (designed 2026-08-12;
phases A-E and the tool halves of F and G landed 2026-08-13). It also records a correction to P2 above: the phase text claims `context_items` are "a second
corpus in `<system-context>` with its own budget", and **there is no prompt path for context items at
all** — the only production consumer of `ContextItem` is the `giap-context` MCP server, and its
toggle ships off. The store, the pipeline and the read-only tools landed; the retrieval surface
landed as a tool (`giap-context__recall`), not as a prompt block.

---

## 5. Invariants

Restated because they are the ones most likely to erode: every item has an owner and a sensitivity;
`Guest` sees nothing; redaction precedes persistence; tokens are encrypted; egress is tracked and
gated; disconnect deletes; nothing leaves.

---

## 6. Deliberate deferrals

- **Write-back.** A separate design with a separate risk profile.
- **Realtime push subscriptions** (Gmail watch, Graph subscriptions). Meaningful latency win,
  meaningful reliability cost. Revisit once P4-P6 have run in a real home for a while.
- **Full-text search over attachments.** Needs a document extraction pipeline; PDF and DOCX parsing
  is its own project.
- **Cross-source entity resolution** (recognising that an e-mail sender and a contact are the same
  person). Wanted, and much easier once several sources are live.

---

## 7. Verification

- **Unit** — idempotent ingest on `(source_id, external_id)`; cursor advance and resume; redaction
  applied before persistence, asserted by inspecting the stored row.
- **Isolation** — two profiles, two mail sources; assert profile A's assistant cannot retrieve
  profile B's items through chat, memory recall, or the `giap-context` tools. Three paths, three
  tests.
- **Guest** — assert zero context items reach a `Guest` session.
- **Egress** — every connector request appears in the event log with a classified host; with
  `network_mode = "allowlist"`, an unlisted host is refused with an actionable message.
- **Token handling** — assert connector tokens never appear in `GET /settings`, any log line, any
  event attribute, or any prompt.
- **Deletion** — disconnect a source, assert items are gone and the count reported matches.
- **Integration** — a wiremock Gmail returning three messages produces three `ContextItem`s, one
  `BusEvent::Ingest`, and a retrievable answer to a question about their content — with the
  assertion that no outbound request carried any conversation text.
