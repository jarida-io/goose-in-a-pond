# PAI-1 — Identity and hard profile boundaries

Requirement: *hard profile boundaries.* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: none — this is the substrate everything else personal stands on.

Verified against code 2026-08-03.

> Status (2026-10-23): P1-P10 are landed and the phase stamps below hold against the code.
> Three gaps remain outside the phase list: `get_session_messages` and `get_session_attachment` take
> no `Principal`; the `giap-memory` MCP tools search at `ProfileScope::Household`
> (`pond-mcp-server/src/memory.rs`); and `language` is collected at onboarding but never written to
> `profiles.preferences` (section 4b). Programme summary: [checklist section 0](./00-checklist.md).

---

## 1. What is true today

### 1.1 The type exists and does almost nothing

`Profile` — `crates/pond-core/src/user_data/domain/profile.rs` — six fields (I miscounted this as
five originally; the quoted block below has always shown six):

```rust
pub struct Profile {
    pub id: String,
    pub display_name: String,
    pub avatar_emoji: String,
    pub preferences: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

No role, no PIN, no auth binding. The doc comment calls it "a household member". Port at
`user_data/ports/profile.rs:9-19`, adapter `pond-infra/src/sqlite_profile.rs`, table from
`migrations/system/0003_profiles.sql`. REST at `routes.rs:102-103,165-166` — note `POST /profiles`
and `PATCH /profiles/{id}` are on the **public** allowlist (`middleware/mod.rs:193,199-200`) for
onboarding.

### 1.2 Memory is one global pool

`MemoryFragment.profile_id: Option<String>` exists (`user_data/domain/memory.rs:100-101`) and
`sqlite_memory.rs:45,109,192-198` genuinely branches on it. Every production caller passes `None`:

| Call site | Function |
|---|---|
| `pond-adapters-goose/src/goose_agent.rs:1643` | `search_similar(&query_vector, None, limit)` |
| `pond-adapters-goose/src/goose_agent.rs:1674` | `search_by_content(&keywords, None, limit)` |
| `pond-adapters-goose/src/goose_agent.rs:2109` | `search_recent(None, limit)` |
| `pond-mcp-server/src/memory.rs:100,133,372,443` | recall / list / forget paths |
| `pond-mcp-server/src/memory.rs:292` | writes fragments with `profile_id: None` |
| `pond-core/src/user_data/services/memory_extraction.rs:84,206` | dedup window and semantic neighbours |

So the column is a schema that has never been used. Every household member's memories are visible
to every other one, and extraction attributes nothing.

### 1.3 Sessions have no owner — but the column has been there all along

*(Corrected 2026-08-04, while implementing P2. The original claim below was "there is no
`profile_id`", which is wrong, and wrong in a way that matters.)*

`Session` — the domain struct — was `id`, `title`, two token counters, `model_name`, `created_at`,
`updated_at`. No `profile_id` **field**.

The **column**, however, has existed since `0003_profiles.sql`, which created the `profiles` table
and bolted `profile_id TEXT REFERENCES profiles(id)` onto `sessions` in the same file. The domain
type never mapped it and `sqlite_session_storage.rs` never selected or wrote it. It was a **dead
column** for thirty-four migrations.

That is the same shape as the two other findings in this document — `memory.profile_id` written as
`None` at every call site, and an identification endpoint whose output nothing reads. The schema
keeps anticipating identity and the code never arrives. It also means P2 is smaller than specified:
wiring, not a column addition.

**And it carried a live trap.** The column was declared with no `ON DELETE` action, which SQLite
reads as `NO ACTION`, while `Database::init` sets `PRAGMA foreign_keys = ON`. Harmless only while
the column stayed NULL — the moment anything writes it, `DELETE FROM profiles WHERE id = ?` starts
failing for any member who has ever spoken to the pond. Wiring the dead column without noticing this
would have broken member deletion. See 3.2 for the fix.

### 1.4 The identification endpoint writes to a map nobody reads

*(FIXED 2026-08-04 by P2. Kept, because the shape of the mistake is the point.)*

`POST /sessions/{id}/identify-user` and `GET|DELETE /sessions/{id}/user` wrote into
`AppState.session_user_bindings: Arc<RwLock<HashMap<String, String>>>`. Grep showed the map touched
at exactly three places outside test fixtures — the three handlers themselves. **No chat, prompt,
memory or tool path consulted it**, and it was process-local, so it died on restart.

The endpoint therefore answered "who is in this session" with whatever it had last been told, by
itself, since the last reboot. It had one caller: the same map's setter.

P2 deletes the map and points all three handlers at `sessions.profile_id`. The binding is durable,
carries its provenance, and is readable by anything that can reach session storage — which is what
makes P3 able to consult it on the chat path. No consumer exists yet; the map's replacement being
persistent is not the same as it being *used*, and this document does not claim otherwise until P3
lands.

### 1.5 No profile reaches the model at all

*(Corrected 2026-08-04. The original claim -- that the primary member's context reaches the prompt
and other members' does not -- was too generous by one step.)*

`chat_stream_inner` does build a `ProfileContext { preferred_name, birthday, language,
atypical_speech }` from `settings.primary_profile_id`. It then binds the resulting prompt as
`let mut _system_prompt` -- underscore-prefixed, deliberately unused -- and `AgentRequest` has no
system-prompt field, so the agent builds its own. On the adapter side,
`build_prompt_partition(&settings, None, ...)` passes `None` for the profile, with a TODO saying to
wire it "when profile port is available".

So **nothing profile-derived reaches the model on either engine path.** The renderers are real, the
guard test `profile_context_goes_to_dynamic_suffix` is real, and neither is reachable from a live
turn. `build_system_prompt_from_template_with_profile` has zero call sites anywhere.

That makes P6 "wire it at all", not "switch its source".

**The placement invariant does hold**, which is the good news: profile context and memories both
ride the user message's `<system-context>` block, never the system prefix, so switching speakers
mid-session costs no re-prefill. One latent risk -- `atypical_speech` is injected into the Tera
context used to render the *static prefix*, so a template that ever wrote `{% if atypical_speech %}`
would bake one profile bit into `prefix_hash`. No template does today.

### 1.6 Face recognition is real; speaker identification is not

`pond-adapters-face-onnx` is a complete implementation — ArcFace-512 / MobileFaceNet-128 via `ort`,
SCRFD and UltraFace detectors, Umeyama alignment, and a Silent-Face MiniFASNet anti-spoofing
ensemble (`antispoof.rs`, `antispoof_onnx.rs`). Raw images are never retained; only float vectors
(`user_data/domain/face_recognition.rs:1-5,21-22`). Twelve REST routes at `routes.rs:279-296`,
per-profile thresholds in migration `0014`.

Speaker identification is designed in [data_pipeline.md, Biometric User Differentiation](../data_pipeline.md#biometric-user-differentiation) — `speaker_id.rs`,
a `speaker_embeddings` table, `diarization_logs`, `biometric_audit_log`, a
`pond-adapters-speaker-embed` crate. **None of it exists.** `delete_user_biometrics` already carries
a comment saying `voice_prints_deleted` "will be populated once Phase 1 lands" (`routes.rs:11207`).

### 1.7 The enforcement hook is already designed

`security/ports/policy.rs:38-111` defines eight scope constants — `MEMORY`, `SETTINGS`, `SECRETS`,
`PROFILE`, `SESSION`, `SCHEDULE`, `SENSOR`, `CAMERA` — plus
`Principal` / `PrincipalKind { Loopback, Token, Internal }`. This is the right shape and it is
inert (see [PAI-2](./02-privacy-and-security-guardrails.md)).

---

## 2. The gap

GIAP can tell you who is in front of the camera and cannot use that fact for anything. It stores a
`profile_id` on every memory and always writes `None`. It has an identification endpoint whose
output is discarded. The product claim "your household assistant" is currently "your household's
shared assistant, and everyone reads everyone's mail".

---

## 3. Design

### 3.1 `ProfileScope` — the unit of ownership

New in `pond-core/src/user_data/domain/profile.rs`:

```rust
/// Who a piece of data belongs to.
pub enum ProfileScope {
    /// One member. Only that member (and an explicitly authorised principal) may read it.
    Owner(String),
    /// Deliberately shared with the whole household — the shopping list, the thermostat.
    Household,
    /// An unidentified speaker. Readable by nobody; discarded on session end unless promoted.
    Guest,
}
```

Carried by memory fragments, sessions, drafts, schedules, notifications and (in
[PAI-8](./08-personal-context-streaming.md)) context items. `Household` is not "no owner" — it is a
positive assertion, which is what makes `NULL` meaning "unclassified legacy row" safe to treat
conservatively during backfill.

### 3.2 Sessions get an owner and a provenance

Migration `0037_session_identification.sql`. As landed — the `profile_id` line the original design
called for is **not** in it, because 1.3 explains the column already exists:

```sql
ALTER TABLE sessions ADD COLUMN identification_source TEXT;      -- paired_device | explicit | face | unknown
ALTER TABLE sessions ADD COLUMN identification_confidence REAL;  -- face matches only

CREATE TRIGGER IF NOT EXISTS trg_profiles_delete_releases_sessions
BEFORE DELETE ON profiles
BEGIN
    UPDATE sessions SET profile_id = NULL, identification_source = NULL,
                        identification_confidence = NULL
     WHERE profile_id = OLD.id;
END;
```

`identification_source` matters as much as `profile_id`. A session bound because a paired phone
presented that member's token is a much stronger claim than one bound by a 0.62-confidence face
match, and the policy layer must be able to tell them apart. This replaces
`AppState.session_user_bindings` entirely — the in-memory map is deleted, not wrapped.

Confidence is stored **only** for `face`. A paired-device binding is not "confidence 1.0" — it is a
different kind of claim, and giving it a number invites somebody to average the two.

**The trigger is not decoration.** It stands in for the `ON DELETE SET NULL` the original design
assumed it could declare, which SQLite will not let us add to an existing column without rebuilding
`sessions` — the hottest table in the database. A `BEFORE DELETE` trigger runs before the row is
removed, so nothing references the profile by the time the delete is applied, and the semantics are
identical. The session survives, stripped of its attribution; erasing the member's *content* is the
separate, deliberate cascade in P7.

Strength ordering is domain logic, not adapter logic: `SessionIdentity::supersedes` refuses to let a
weaker source take over a session a stronger one bound. The case it exists for is concrete — a
member's phone opens a session, then the room camera sees whoever walks past, and without the check
a face match silently overwrites a cryptographic binding with a probabilistic one about a different
person. Equal strength *does* supersede, so re-identification still works.

Resolution order when a turn arrives:

1. **Paired device** — the bearer token maps to a device, the device maps to a profile. Strongest.
2. **Explicit** — the user said "this is Liz" or picked a profile in the UI.
3. **Face** — a `FaceIdentification` above the per-profile threshold, anti-spoof passed, within a
   freshness window.
4. **Unknown** → `Guest`.

Rung 1's second hop — *device maps to a profile* — is `devices.profile_id`, added by P9 on
2026-08-10 and read through the `DeviceAttribution` port. The first hop, bearer token → device, has
always existed (`session_tokens.device_id`, and `Handshake::client_id_for_token` names the client).
**Neither hop is wired into a turn yet**; see P9's stamp, and the test that fails when that changes.

### 3.3 `Principal` carries the profile

`security/ports/policy.rs` — `Principal` gains `profile_id: Option<String>` and
`identification_source`. This is the single value every enforcement decision in PAI-2 keys on.

### 3.4 The hard boundary

The rule, stated once so every implementation can be checked against it:

> A read returns rows whose scope is `Owner(current)` or `Household`. A write is stamped
> `Owner(current)`, or `Household` only when the caller says so explicitly. Any access to
> `Owner(other)` is a `SecurityPolicy` decision that is **denied by default**.

Concretely, `MemoryRepository::search_*` stop taking `Option<&str>` and start taking a
`&ProfileScope` — so passing "everything" becomes a deliberate, greppable act rather than the path
of least resistance that produced the current `None` everywhere.

Cross-profile access is the **first production call site** for `SecurityPolicy::allow` and
`::audit`. That is deliberate: it gives the inert policy port a real, testable job with a clear
failure mode.

### 3.5 Guest degradation

An unidentified speaker gets: no personal memory injection, no personal-data tool groups
(`giap-memory` reads are scoped to `Household`), no draft approval rights, and no ingested context
items. It still gets weather, time, device control and knowledge tools — the assistant stays useful
to a visitor without becoming a disclosure channel.

This is the behaviour that makes voice safe before speaker identification exists.

### 3.6 The prompt carries the current member, not the primary one

`ProfileContext` is built from the session's resolved profile. `settings.primary_profile_id` becomes
the fallback for `Household`-scope and unattributed contexts only.

**KV-prefix constraint:** profile context already rides the user message's `<system-context>` block,
not the system prefix — that placement is correct and must not change (see the programme's
invariant 1). Switching speakers mid-session therefore costs nothing in prefill.

### 3.7 Deletion actually deletes

`DELETE /users/{profile_id}` today clears biometrics only (`delete_user_biometrics` in `routes.rs`).
It must cascade memories, sessions, drafts, schedules and context items owned by that profile, and
report counts per category. `Household`-scoped data survives — deleting a member does not delete the
shopping list.

Most of the cascade already exists, found while implementing P1 and P2 rather than during the design
pass. Every `profile_id` foreign key in the schema, and what it does on delete:

| Table | Migration | On delete |
|---|---|---|
| `memory_fragments` | `0005` | `CASCADE` |
| `face_embeddings` | `0013` | `CASCADE` |
| `face_profile_thresholds` | `0014` | `CASCADE` |
| `sessions` | `0003` | **nothing declared** -- so `NO ACTION`, until `0037`'s trigger |

`sessions` was the only one of the four written without an action, which is exactly why it was the
one that would have broken. A member's memories, faces and thresholds already vanish with them; P7
has to *count* those, not delete them. The session releases to NULL instead, deliberately: a
conversation is not solely the speaker's, and P7's job there is to decide what of its content goes,
not whether the row does.

So P7 is narrower than written — drafts, schedules, and the per-category counts. But the audit above
is the part to keep: three of the four behaviours were decided years ago by migrations nobody
remembered writing, and the fourth was decided by omission.

### 3.8 Speaker identification is scoped out, and said so

Voice is a shared surface. Until `pond-adapters-speaker-embed` exists, a voice session resolves to
`Household` (if a face is co-present and confident) or `Guest`. **This document does not claim
per-person voice memory works.** The speaker-ID design in [data_pipeline.md, Biometric User Differentiation](../data_pipeline.md#biometric-user-differentiation) is the
follow-on; PAI-1 makes it a drop-in by putting `identification_source` in place first.

---

## 4. Phases

- **P1 — LANDED 2026-08-04.** `ProfileScope` domain type; `MemoryRepository` signature change from
  `Option<&str>` to `&ProfileScope`; all current call sites pass `ProfileScope::Household` so
  behaviour is **byte-identical** on landing. This is the refactor that makes every later phase
  small.
- **P2 — LANDED 2026-08-04.** Migration `0037_session_identification.sql` (provenance columns and
  the delete trigger — `profile_id` was already there, see 1.3); `Session.profile_id`,
  `SessionIdentity`, `IdentificationSource`, and the two `SessionStorage` methods that read and
  write them; `AppState.session_user_bindings` deleted and its three handlers repointed at the
  column.

  P2 deliberately does **not** add a `SessionIdentity -> ProfileScope` conversion. Every session in
  every existing pond is unattributed, so such a method would have to answer "what scope is an
  unidentified session" today — and the only behaviour-preserving answer, `Household`, is precisely
  the scope-widening default invariant 2 calls a bug. That question belongs to P3, which decides it
  with the paired-device, explicit and face inputs in hand rather than from the stored row alone.
- **P3 — LANDED 2026-08-04.** Resolver in `user_data/services/identity_resolution.rs`; resolved once
  per turn at the API edge by `resolve_turn_scope` and carried on `AgentRequest.profile_scope` into
  the adapter, which uses it for all three memory searches. `PUT /sessions/{id}/user` added, so
  `Explicit` finally has a producer.

  **Resolved at the edge, carried down, never recomputed.** Two resolutions of one turn could
  disagree, and the one nearer the data would silently win. `AgentRequest` was the right channel —
  `voice_mode` and `canvas_mode` are the same pattern — and the field is non-optional in Rust with
  no `Default` on the struct, so all **twelve** construction sites had to state a scope. The
  compiler found one the recon pass had missed (`shared/services/delegation.rs`). Every failure in
  `resolve_turn_scope` narrows: a storage error reads as unidentified, a profile-count error assumes
  more than one member.

  **Deliberately not used:** the process-global `set_current_session_id` / `current_session_id` pair
  (`shared/services/egress.rs`). It is a `RwLock<String>`, not a task-local, and the SSE semaphore
  permits more than one concurrent stream — so extending it for identity would attribute one
  speaker's turn to another under load. That is precisely the failure this workstream exists to
  prevent.

  Original phase text, for the record:

- **P3 (as designed) — resolver LANDED 2026-08-04, wiring outstanding.** `user_data/services/identity_resolution.rs`
  holds the chain as one pure function with `ResolutionInputs` / `ResolvedIdentity`. No call site
  consumes it yet; threading it through `AgentRequest` into `ChatService` is the remaining half.

  **The strongest rung of the chain cannot be built.** Nothing in the schema links a paired device
  to a household member: `session_tokens` stores `device_id` and `client_id` and no profile, and
  neither do `push_tokens`, `pairing_codes` or `handshake_challenges`. The pairing flow never asks
  who is pairing. So `paired_device_profile` is an input the resolver honours and every caller feeds
  `None`.

  Deliberately **not** worked around. Falling back to `settings.primary_profile_id` would attribute
  every phone in the house to one person — the wrong-attribution failure this whole workstream
  exists to prevent, and worse than admitting we do not know. Capturing a member at pairing time is
  its own change, and it is also what [PAI-7](./07-proactive-intelligence.md) needs before it can
  address a notification to "that profile's devices"; that document assumes the link exists.

  > **The schema half is no longer true, 2026-08-10.** P9 below adds
  > `devices.profile_id` and `pairing_codes.profile_id` (migration 0043) and the
  > `DeviceAttribution` port that reads them, so the link now exists in storage. The
  > *behavioural* half of the paragraph above still stands exactly as written: no production
  > caller supplies `paired_device_profile`, because the handler that would capture the member
  > lives in `routes.rs`. Read P9's stamp before citing either half.

  **What an unidentified speaker resolves to.** `Household` while the pond has one member, `Guest`
  once it has more than one. This looks like a fudge and is not: `Household` and `Guest` differ only
  when there is somebody to be excluded from, so in a one-member pond they describe the same rows.
  Resolving to `Guest` unconditionally would make a working single-user assistant refuse to remember
  anything about its only user, which is a regression rather than a boundary. It does mean **adding
  a second household member is the moment a pond's privacy posture changes**, which belongs in
  release notes.

  There is also no explicit-identification route yet — `POST /sessions/{id}/identify-user` is
  face-only — so `Explicit` has no producer either. That is a small addition and it lands with the
  wiring.
- **P4 — write side LANDED 2026-08-04; policy layer still designed.** Memory extraction now stamps
  `profile_id` from the turn's scope, and refuses to write anything at all for a `Guest`.

  **This was the phase the whole workstream turned on, and an audit found it missing.** Until it
  landed, every production write path set `profile_id: None`, so `scope_sql`'s `Owner(id)` predicate
  -- `profile_id = ? OR profile_id IS NULL` -- matched exactly the same rows as `Household`. The
  read-side scoping in P1 and P3 was real plumbing with nothing flowing through it, and no test
  caught it because the only fixtures producing an owned row set `profile_id` by hand, a state no
  production path could reach. Three tests now assert attribution end to end, including that a
  household turn stays unattributed so shared context survives a member's deletion.

  **P4 COMPLETE 2026-08-05.** `security_policy_mode` (`off | audit | enforce`, default `audit`),
  `PolicyMode` / `PolicyDecision` in the port, and a first production call site.

  **The deny matrix was not built as specified, deliberately.** Section 3.1 of PAI-2 asks for eight
  scopes x three `PrincipalKind`s. Worked through cell by cell, all twenty-four are `allow`: every
  kind legitimately needs every scope for something that exists in the code today, and denying
  `Internal` anything breaks background work silently rather than returning an error to anybody. A
  matrix of twenty-four allows is not a control, it is a table that looks like one. **The axis that
  discriminates is whether a caller has *proved* the identity it claims** — `PrincipalKind` cannot
  express that, which is why the matrix keyed on it comes out empty.

  So the rule is `is_identity_assertion_proven`, and its call site is `PUT /sessions/{id}/user`,
  which **took a `profile_id` from the request body and bound it at `Explicit` strength with no
  ownership check at all**. Any paired device could declare itself any household member; every
  subsequent turn then resolved to that member's scope and injected their memories. That is
  cross-profile access laundered through the session row rather than through a query parameter, and
  it is the honest first production call site 3.4 asks for. The memory reads were not: `Owner` is
  constructed in exactly one place, `identity_resolution::resolve`, always from the session's own
  binding — so a check there would have had nothing to deny.

  **`allowed` and `denied_reason` are separate, and that is the load-bearing detail.** In `audit` a
  refusal does not block, so `allowed` is `true` for exactly the requests `enforce` would have
  stopped. An audit entry recording only the effect would read "permitted" for all of them and could
  not answer the one question the mode exists to answer. `PolicyDecision::verdict()` reports
  `allow` / `would_deny` / `deny`.

  **Nothing can prove an identity yet**, so `enforce` currently refuses every remote explicit
  identification. That is the correct reading, and it is why the default is `audit`: the missing
  rung is the device-to-member link, which is its own phase.

  `Principal` gains `proven_profile_id` (3.3) and is constructed in production for the first time,
  in `auth_middleware`. That required `Handshake::client_id_for_token`, defaulted to `Ok(None)` so
  no existing implementor changes — `validate_token` answers only yes/no, so a handler could know a
  request was authenticated and still not know who sent it, which is why no `Principal` had ever
  been built.

  **The identity write is now race-free.** `set_session_identity_if_stronger` does the rank
  comparison inside the `UPDATE`, building the `CASE` from `IdentificationSource::ALL_RANKED` so the
  ordering stays domain policy and a test pins the two together. The read-compare-write it replaced
  could lose: two requests both read `Unknown`, both passed `supersedes`, and the later write won
  whatever its rank -- so a face match landing a millisecond after somebody tapped "this is Liz"
  took the session, for a different person, on weaker evidence. Reachable in production, and my
  original comment claimed the only loser was a competing face match on the same camera frame.

  **The `giap-memory` bypass is closed at the tool-group layer, not in the MCP server.**
  `recall_memories`, `keyword_search` and `forget_memory` still pass `ProfileScope::Household`
  directly, because `MemoryMcpServer` is process-global with no per-turn session. So the fix is one
  layer up: a `Guest` session is never given the group, and cannot call the tools at all. See P5.

  Threading a scope into the MCP server per turn remains worth doing -- it would also scope an
  *Owner*'s tool calls, which the group gate does not -- but the only mechanism available today is
  the process-global `set_current_session_id`, a `RwLock<String>` that the SSE semaphore already
  permits more than one turn to race on. Using it would trade a guest hole for a misattribution
  bug. Deferred until there is a per-turn cell to hang it on.

  > **The "only mechanism available" clause was wrong. Corrected 2026-08-05 by PAI-2 P1.** There is
  > a race-free per-call channel and it needs no Goose patch: Goose injects `agent-session-id` into
  > every `CallToolRequest`'s `Meta`, rmcp serialises extension-level `Meta` as the wire `_meta` and
  > swaps it into the `RequestContext.meta` handed to the tool handler. `giap-draft` reads it
  > (`crates/pond-mcp-server/src/session_meta.rs`) and resolves a speaker from it. The verdict on
  > `set_current_session_id` stands -- it is still a raced global and still must not be used for
  > identity -- but "deferred until there is a per-turn cell" is satisfied, and `giap-memory` can
  > adopt the same reader.
- **P5 — landed 2026-08-04, INERT until 2026-08-05.** A `Guest` turn gets no memory injection
  (read), deposits no memory (write), and is never given the personal-data tool groups.

  **The tool-group half did not work on any default install, for a day.** The subtraction sits
  inside `resolve_session_tool_groups`, which is only reached from the
  `settings.tool_selection_is_relevant()` branch in `goose_agent` — and `default_tool_selection_mode()`
  returns `"all"`. The `else` branch returns `allowed_tools` untouched, and *that* set is what gets
  published to the provider shim. So on a default pond a `Guest` kept `giap-memory`, `giap-draft`,
  `giap-audit`, `giap-vision` and `giap-sensors`, and could recall, keyword-search or
  `forget_memory` the entire household — the exact hole this phase was written to close, left open
  by the phase that closed it.

  Fixed by subtracting at the **tool** level after both branches converge
  (`tool_selection::subtract_guest_denied_tools`), which is the only point every mode passes
  through. Idempotent, so the group-level pass stays — it still keeps withheld groups out of the
  dormant-groups note.

  **Why it was missed, which is the part worth keeping:** the phase was verified against the code
  path it added, not against the default configuration. Every test exercised the `relevant` branch
  because that is the branch the feature lives in. A guard now asserts that every name on the
  denylist is a real catalog extension, because a typo there is invisible in exactly the same way —
  the filter would simply never match, and the denial would look present while protecting nothing.

  `groups_denied_to_guests()` in `mcp/domain/tool_group.rs` names them; read the list there rather
  than from here. `giap-draft`, `giap-audit` and `giap-vision` were on it until those groups were
  deleted on 2026-09-10. The adapter subtracts them **after** `select_groups`, because `giap-memory`
  is `core` and selection puts core groups back unconditionally -- filtering the candidates going in
  would not stick.

  This is the layer that actually closes the MCP hole. Suppressing memory injection stops a guest
  being *told* anything; removing the tools stops the model being *able* to look. `recall_memories`
  and `forget_memory` carry no session of their own, so there is nowhere lower to check.

  Deliberately a **denylist**, not an allowlist: a new group is far more likely to be neutral than
  personal, and a new *personal* group is exactly the change whose author should have to think about
  this list. An allowlist would silently deny every new group to guests and surface as a bug report.
  A guest keeps weather, knowledge, device control, the toolkit and system time -- useful to a
  visitor without being a disclosure channel.

  Separately, and belonging to [PAI-2](./02-privacy-and-security-guardrails.md) rather than here:
  **`approve_draft` performs no ownership check at all.** Any session can approve any draft id. The
  group gate keeps a *guest* away from it; it does nothing about one member approving another's.

  > **CLOSED 2026-08-05 by PAI-2 P1**, where Jerry decided it belonged. Drafts have an owner
  > (migration 0038), the caller is resolved from the engine session in the MCP request `_meta`, and
  > `is_draft_decision_permitted` refuses a foreign draft -- audited under `audit`, blocked under
  > `enforce`. `list_drafts` was scoping by a model-supplied `session_id` that defaulted to
  > `"default"`, which is what made a foreign draft id enumerable in the first place; that is fixed
  > in the same change.
- **P6 — LANDED 2026-08-04.** `profile_context_for` builds the context from the resolved scope:
  `Owner(id)` gives that member's preferences, `Household` falls back to
  `settings.primary_profile_id`, and **`Guest` gets `None`** -- falling back to the primary member
  there would greet a stranger by the owner's name. It rides `AgentRequest.profile_context` and the
  adapter passes it to `build_prompt_partition`, replacing the `None` and its TODO.

  KV-prefix safe: profile lines land in the dynamic suffix, which rides `<system-context>` in the
  user message. A speaker switch mid-session costs no re-prefill.
- **P7 — LANDED 2026-08-04, narrower than designed.** `DELETE /profiles/{id}` now checks existence
  (it returned 204 for an id that never existed, which made "did I delete the right person"
  unanswerable), counts before deleting, and reports per category. Sessions are reported under
  `released`, not `deleted` — a conversation is not solely the speaker's.

  **The design's "cascade drafts and schedules" cannot be built.** `drafts` has no owner column at
  all — it is keyed by `session_id` with no foreign key — and **there is no `schedules` table**; the
  scheduler is `tokio-cron-scheduler`, in-process. Both would need an owner adding first.

  > **Half of that is no longer true, 2026-08-05.** PAI-2 P1's migration 0038 gives `drafts` a
  > `profile_id`, and a `BEFORE DELETE ON profiles` trigger expires a departed member's pending
  > drafts and then releases the column — so the drafts cascade exists, landed with the ownership
  > rule that needed the column anyway rather than as a separate pass. It is a trigger, not an
  > `ON DELETE` action, because 0018 declared no foreign key and adding one means rebuilding the
  > table; the delete was never going to fail, so the trigger is there for the semantics. The
  > schedules half stands: there is still no `schedules` table.

  **One real bug fixed on the way.** `settings.primary_profile_id` is a key-value row, not a foreign
  key, so no cascade can reach it. Deleting the primary member left an id pointing at nobody — and
  the single production reader silently got `None` from the lookup, so the dangling reference never
  surfaced. It is cleared before the delete now, with a test in each direction.
- **P8 — LANDED 2026-08-04 as a no-op, deliberately.** The semantics this phase asked for are
  already what `scope_sql` does: `Owner(id)` matches `profile_id = ? OR profile_id IS NULL`, so a
  legacy row reads as shared household context, and `count_for_profile` matches `profile_id = ?`
  exactly, so nobody *owns* one. A member's deletion therefore cannot take shared context with them.

  **No migration was written.** An `UPDATE` would only stamp a value into rows whose meaning is
  already correct without it, and it would convert "never attributed" into "positively assigned",
  destroying the distinction the next phase may need. The phase is closed by three tests pinning the
  behaviour rather than by SQL — which is the honest form of "already true".
- **P9 — the device-to-profile rung. STORAGE AND DOMAIN LANDED 2026-08-10; THE CAPTURE HALF REACHED
  PRODUCTION 2026-08-11; THE DELIVERY HALF IS STILL UNREACHED.** Read all three clauses before
  relying on this phase for anything.

  **Updated 2026-08-11, at the demand of the guard that was written to demand it.** The loopback
  issuance route now calls `issue_pairing_code_for`, so `devices.profile_id` is reachable in
  production for the first time and a paired device can be somebody's. `handshake_issue_pairing_code`
  keeps `if !peer.ip().is_loopback() { return FORBIDDEN }` as its FIRST statement, before the body is
  read — re-checked when this stamp changed, because the whole security argument for capturing the
  member at issuance rests on it. That argument, restated so nobody reopens it: a `profile_id` in the
  pairing REQUEST would be a client naming its own owner, and
  `IdentificationSource::PairedDevice` outranks both face and explicit identification, so the claim
  would outrank every proof the pond can actually make. At issuance the answer comes from somebody
  standing at the pond instead. An empty body still means an unattributed code, which is what the
  CLI and the dashboard have always sent; a body that is present and unreadable is REFUSED rather
  than defaulted, because defaulting would narrow correctly and still tell an operator who meant to
  bind the code to Liz that it worked.

  **The DELIVERY half reached production 2026-08-11 too.** `main.rs` builds a
  `SqliteDeviceAttribution` and hands it to `BroadcastNotificationSender::with_device_attribution`,
  so `send_to_profile` resolves a member's devices instead of answering `AttributionUnavailable`,
  which is what it did on every pond until that line. Be exact about what that buys: the path is
  FUNCTIONAL and still UNCALLED. Nothing outside a test calls `send_to_profile`; all four production
  notification producers still `broadcast()`, which is right for three of them — a schedule
  completing, a rule firing and a pairing notice are household facts — and the fourth, the one that
  makes this path matter, is PAI-7 P4's reviewer delivering a proposal to the member it is addressed
  to. P4's domain is landed; its background loop is not.

  **The IDENTITY half is what is left of P9, and it is not a wiring job.** `resolve_turn_scope`
  still passes `paired_device_profile: None`, and `no_turn_resolves_a_paired_device_to_a_member_yet`
  still passes because of it. The obstacle is that a turn does not know its device:
  `session_tokens.device_id` exists and is indexed, and `client_id_for_token` already reads that
  table, but the auth layer surfaces neither to a handler. Closing it means a `device_id_for_token`
  on the `Handshake` port, the middleware putting it where a handler can see it, and
  `resolve_turn_scope` taking it — at which point `IdentificationSource::PairedDevice` starts
  outranking face and explicit identification on every turn, which is why it is worth doing
  deliberately rather than opportunistically.

  This is the phase `00-checklist.md`'s 2026-08-05 entry called for — "capturing a household member
  at pairing time gets its own phase" — and it never ran. PAI-1's row saying `COMPLETE` is why:
  the ledger had nothing left to warn anybody with. Meanwhile `identity_resolution::resolve`'s
  strongest rung was decorative, and PAI-7 section 3.4's chain — profile → paired devices → push
  tokens — did not exist at all, so its P5 could only broadcast a targeted proposal or deliver
  nothing.

  **Landed.** Migration `0043_device_profile.sql` adds `devices.profile_id` and
  `pairing_codes.profile_id`; `user_data/ports/device_attribution.rs` (the `DeviceAttribution` port
  plus `checked_profile_id`); `pond-infra`'s `SqliteDeviceAttribution`; `PairingCode.profile_id` and
  `Handshake::issue_pairing_code_for`; and `SqliteHandshakeAdapter::verify_handshake` binding the
  device to whoever the consumed code named.

  **Not landed, and this is the part that matters.** No production code constructs
  `SqliteDeviceAttribution`, and no route passes a member to `issue_pairing_code_for`. The two call
  sites that will reach it are `handshake_issue_pairing_code` in `crates/pond-api/src/routes.rs`
  (the loopback issuance route, which has to offer the operator the choice of member) and PAI-7 P5's
  delivery path, which needs `push_tokens_for_profile`. Both live in files another session held when
  this landed. **A domain-only phase is legitimate — PAI-6 P1 was one — but only when it says so**,
  and this programme has three recorded cases of a phase stamping itself as working while inert. The
  usual defence does not apply here: `dead_code` cannot fire for `pub` items in a library crate, so
  its silence proves nothing.

  So the claim is executable instead. `crates/pond-infra/tests/device_profile_rung_is_not_wired_yet.rs`
  asserts that `routes.rs` still writes `paired_device_profile: None` at every occurrence, that
  neither `main.rs` nor `routes.rs` names `DeviceAttribution` or any of its methods, and that
  `routes.rs` does not call `issue_pairing_code_for` or read a profile off the pairing request. Each
  failure message names this stamp. The day the rung is wired, the build breaks and somebody has to
  rewrite this paragraph rather than remember to.

  **NULL means unattributed, and the two directions off that column are deliberately not mirror
  images.** For identity it means *I do not know*: the resolver falls through to the next rung, which
  is today's behaviour and a narrowing. For delivery it means the device is **nobody's**:
  `devices_for_profile` matches `profile_id = ?` and therefore returns no unattributed device, so a
  targeted proposal reaches nobody rather than every unclaimed screen in the house. Reaching the
  shared kitchen tablet stays the broadcast path's job, which is honest about being a broadcast.

  That is a different call from the one invariant 4 makes for memory, where an unattributed fragment
  is `Household` by positive classification — and the difference is worth stating because the two
  look like the same question. A memory is content; a device is a destination. Reading shared content
  discloses nothing new, while delivering a member's proposal to an unclaimed screen in a shared room
  is precisely the disclosure this workstream exists to prevent. "Deliver to nobody" and "deliver to
  everybody" are both failures and only one of them is a leak.

  **The member is captured at code ISSUANCE, not in the pairing request**, and this is the load-bearing
  design decision. A `profile_id` on `VerifyRequest` is the same shape as the live hole P4 closed on
  `PUT /sessions/{id}/user` — a `profile_id` taken from the request body with no ownership check —
  and it would be worse here, because `IdentificationSource::PairedDevice` outranks both face and
  explicit, so a client-asserted profile would not merely be unproven: it would outrank every proof
  this pond can actually make. A pairing code is minted on the host (`handshake_pairing_code` and
  `handshake_issue_pairing_code` both refuse a non-loopback peer inside the handler), so binding the
  member there means the answer comes from somebody standing at the pond. The code already has
  single-use, expiry and hash-only-at-rest semantics and the attribution inherits all three.
  `verify_handshake` writes `profile_id = excluded.profile_id` on conflict, so re-pairing with an
  ordinary code **releases** an attribution rather than letting whoever reuses a self-reported
  `device_id` inherit the previous owner.

  **The delete rule, declared inline rather than as a trigger.** P2 could not do that: 0003 had
  already declared `sessions.profile_id` with no action and SQLite cannot add one in place, so 0037
  needed a `BEFORE DELETE` trigger. A column added now carries `ON DELETE SET NULL` itself. Verified
  with the `sqlite3` CLI against a database with 0001–0042 applied **and rows in it**: existing
  devices and codes read NULL and keep working, and attributing a device then deleting the member
  leaves the phone present, its push token intact, `profile_id` NULL, and `pragma_foreign_key_check`
  empty. Deleting a household member must not fail because they owned a phone, and must not leave
  that phone pointing at a ghost; both halves hold, in SQLite and in
  `deleting_a_member_releases_their_devices_and_keeps_the_phone_working`.

  **No backfill.** Every existing device becomes unattributed, which is the truth — no pond has ever
  captured who was pairing. Stamping `settings.primary_profile_id` into them would attribute every
  phone in the house to one person, the failure P3 refused for the same reason.

  **Known wart, recorded rather than fixed.** The legacy single-shot `handshake()` path writes no
  `devices` row at all, so a legacy pair is always unattributed — narrowing, and left alone because
  registering a device there is a behaviour change outside this rung. The consequence is that a
  member-bound code consumed on that path is burned with its attribution discarded, and the operator
  re-issues.

---

## 4b. P10 — a member's particulars, and who may hear them (2026-08-12)

Two defects in one surface, found by asking why the assistant did not know the user's name.

### The capability did not exist

`particulars_for` reads `preferred_name`, `birthday`, `language` and
`accessibility_atypical_speech` out of `profiles.preferences`. The desktop onboarding collected a
preferred name and birthday and wrote them to **browser `localStorage`** under `giap-user-profile`.
`PATCH /api/v1/profiles/{id}` existed and had no client method. Every piece worked; nothing joined
them, and the model had never seen any of it.

This is the third instance of one shape in this programme — reader, writer, no wire, suite green.
The others were the 22 settings switches that rendered without an `onChange`, and the MCP extensions
registered but unreachable. The lesson that generalises: **a field with a reader and no writer is
indistinguishable from a working feature until somebody looks at the data**, and no unit test on
either side can see it. What catches it is checking the stored row.

### Fixing it would have opened a boundary

`profile_context_for` resolves `ProfileScope::Household` to `primary_profile_id`. So the moment a
writer existed, every unattributed turn would have stated the primary member's name and birthday —
"the user prefers to be called Jerry", "the user's birthday is …" — while somebody else was
speaking. The disclosure was latent for exactly as long as the bug was.

Personal particulars are now `Owner`-scoped. `atypical_speech` deliberately still crosses into
`Household`: it renders as "be patient, never correct speech patterns, interpret incomplete
sentences charitably", which discloses nothing about anybody, and a household that configured it
wants it applied precisely when the pond cannot tell who is speaking. **Being patient with the wrong
person costs nothing; announcing the wrong person's birthday does.**

The rule is extracted as `particulars_for(attributed, prefs)` so it is exhaustively testable without
an `AppState`, a database or a router — it was previously reachable only through all three, which is
why it went unexamined.

### What a later change must not undo

* **The key spelling is a cross-language contract.** The server reads snake_case; the desktop holds
  camelCase. A camelCase key returns 200 from the API, populates the row, and reaches the model as
  nothing — the original bug in a new disguise. Pinned on both sides
  (`camel_case_keys_are_not_read_and_that_is_the_point`, and `PROFILE_PREF_KEYS` in the wizard).
* **Booleans are the strings `"true"`/`"false"`.** `preferences` is `HashMap<String, String>` and the
  server compares against the literal.
* **An empty value is omitted, not written blank.** The prompt builder skips a missing key and would
  render "The user's birthday is ." for a present-but-empty one.
* **A fresh pond has no member.** Nothing ever created a profile or set `primary_profile_id` — this
  pond's was configured by hand — so the wizard now ensures one before writing preferences.
* **The migration must never win a tie.** `localStorage` is a stale copy from one browser on one
  machine; the server is the household's record. An old laptop must not revert a name corrected on
  a phone.

### Still missing

`language` has a collector and no writer. The onboarding locale step (`StepLocale.tsx`) is an explicit
picker that puts it in the onboarding draft, and `PROFILE_PREF_KEYS.language` names the key, but
`buildProfilePatch` (`useOnboardingPersist.ts`) writes only the about-you fields, so the value never
reaches `profiles.preferences` and `Always respond in {language}` is still unreachable in production.
Writing it is one entry in that patch; the UX decision this section used to record (an explicit picker,
not `navigator.language`) is already made by the locale step.

---

## 4c. A device paired to nobody stays nobody's (2026-10-28)

Found from a household on the Orin whose phone greeted "Good night, there" while the pond listed two
people. P9 is intact; what was missing is every way into it after the pond has members.

### What the pond held

Read-only from `pond_system.db`, 2026-10-28:

* Two `profiles` rows, created 2026-10-27 17:20 and 20:54.
* The phone's `devices` row (paired 2026-10-20, a week before any member existed) with `profile_id`
  NULL. The desktop's row, paired by loopback at 20:14 while there was exactly one member, carried
  that member: the sole-member default worked the one time it could.
* `settings.primary_profile_id` = the owner's **display name**, written 21:36 from the dashboard's
  "Household profile" box, which was a free-text field over an id.
* Every pairing code issued after 20:54 logged `attributed=false`.

### Why nothing could fix it

A device's member is written in exactly three places: the pairing code it consumed (P9), the
setup-claim latch (sole member at claim time), and `attribute_claimant_to_owner` at onboarding
completion. This pond was onboarded on 2026-09-17, its phone was not the claimant, and with two
members a code issued with no body binds nobody. `POST /handshake/pairing-code` has always taken
`{profile_id}`, but both of its callers, the dashboard's Pairing page and `pond-server pairing`, sent
no body. So on any pond of two or more members, no shipped UI could attribute any device, and the
log line telling the operator to let "Devices" bind it named a route that takes no member.

The consequence, per turn on that phone: `resolve_turn_scope` gets `DeviceRung::Unattributed`, the
session row carries no identity, and with two members `identity_resolution::resolve` answers
`Guest`. A guest recalls nothing (`excludes_everything`), is not offered `giap-memory`,
`giap-sensors`, `giap-vision`, `giap-context`, delegation or travel (`groups_denied_to_guests`),
hears no unprompted speech, and cannot decide drafts or book rides. Its conversations are not
attributed, so the batch extractor files nothing for anyone. Member-addressed delivery
(`push_tokens_for_profile`) finds no device for the owner except the desktop.

### What changed

* `PUT /settings` refuses a `primary_profile_id` that is not a member's id (422 with `fields`), and
  the dashboard renders it as a picker of members. A stored value that names nobody is shown as
  "not a household member" rather than dropped.
* The Pairing page asks "Who will use this device?" and issues the code for that member, or for
  nobody explicitly (`{unattributed: true}`), and says whose a device pairing with the shown code
  becomes. `pond-server pairing --member <PROFILE_ID>` does the same from a shell.
* Pinned through the routes with no hand-set `profile_id`:
  `a_phone_paired_before_any_member_existed_is_claimed_by_pairing_again_for_its_member` pairs a
  phone before any member exists, creates two, shows the phone is nobody's and a bodyless code still
  binds nobody, then pairs the same install id again with a member's code and reads the member back
  from `GET /devices/self`.

### Still open, and why it is a decision rather than a bug

Linking an already-paired phone still means pairing it again. Three ways to remove that step, each a
choice about who may assert identity, since `PairedDevice` outranks face and explicit:

1. **Host-only re-attribution on Devices.** `PUT /devices/{id}` gains `profile_id`, accepted only with
   the host credential on loopback, the same trust as issuing a member-bound code. Smallest change;
   it moves an identity claim without the phone taking part.
2. **Attribute pre-member devices when the first member is created**, as
   `attribute_claimant_to_owner` does for the claimant. Fixes exactly this household, but on a pond
   whose devices include a guest's or a shared tablet it makes them the first member's, the failure
   section 4's "no backfill" refused.
3. **"This is me" from the phone**, confirmed at the pond (a code read out or shown on the pond's
   screen, as pairing is). Fits a second member's phone best and needs a new confirmation flow.

---

## 5. Invariants

1. Profile context stays in `<system-context>` on the user message. Never the system prefix.
2. A scope-widening default is a bug. Where tool selection widens on failure, access narrows.
3. `identification_source` is never dropped on the floor — a decision made on a 0.6 face match must
   be distinguishable in the audit log from one made on a paired token.
4. `Household` is a positive classification, never a synonym for `NULL`.
5. Deleting a member never deletes `Household` data.

---

## 6. Deliberate deferrals

- **Speaker identification.** Real work, separate crate, biometric-consent implications. PAI-1 makes
  it additive.
- **Per-profile PINs or passwords.** Household members are not adversaries in the threat model; the
  boundary protects against accidental disclosure and cross-contamination of memory, not against a
  determined member with physical access. If that changes, `Principal` is the place to add it.
- **Per-profile model or prompt-style selection.** Interesting, not load-bearing.

---

## 7. Verification

### What P1 and P2 actually ran (2026-08-04)

`cargo fmt --check`; `pond-core` 720 lib tests; `pond-infra` 181; `pond-mcp-server` 176; `pond-api`
105 lib plus all 17 integration targets; `pond-adapters-goose` 103; `cargo check -p pond-server`;
and a live server run against a real data directory.

New tests, and what each would have to be broken for:

| Where | Asserts |
|---|---|
| `session.rs` domain | every source round-trips; an unrecognised stored source degrades to `Unknown` and never upward; the rank order; a face match cannot take over a paired-device session; equal strength still supersedes so re-identification works |
| `sqlite_session_storage.rs` | a new session is unattributed; identity round-trips including the face confidence; writing to a session that does not exist is `SessionNotFound`; reading one is not; **deleting a member with a live session succeeds and releases it**; identity cannot name a profile that does not exist |
| `agent_data_integration_test.rs` | the same, over HTTP -- and specifically that a binding survives the process that made it, which is what the deleted in-memory map could never do |

The integration tests go through the router rather than the repository on purpose. What P2 replaced
was a map that no layer below HTTP ever saw, so a test beneath that boundary would have passed
against the old code too.

The `pond-api` integration targets had to be run **one target at a time**, deleting each ~600 MB
test binary before building the next. Seventeen of them link Goose statically and together exceed
this container's disk allowance. `cargo test -p pond-api` as a single invocation fails with
`No space left on device`, which surfaces as a linker `Bus error` and reads exactly like a code
fault. It is not one.

### What P9 actually ran (2026-08-10)

`cargo fmt --check`; `cargo clippy -p pond-core -p pond-infra`; `cargo test -p pond-core` and
`cargo test -p pond-infra`. **No live server run, and it is owed**: P9 adds a migration, which
section 2.4 of `00-checklist.md` names as one of the four things that require one. The migration was
instead driven by hand with the `sqlite3` CLI against a database with 0001–0042 applied and rows in
it — which covers "does it work on a populated database" and does **not** cover startup ordering or
route registration. The live run belongs to whoever holds the server this round.

New tests, and what each would have to be broken for:

| Where | Asserts |
|---|---|
| `ports/device_attribution.rs` | a blank profile id is refused by name rather than answered; a real one passes through unrewritten (trimming it would make the lookup disagree with the write) |
| `sqlite_device_attribution.rs` | a device starts unattributed, can be claimed and released; a profile yields its devices *and their push tokens*; an unattributed device is in neither answer; deleting a member succeeds, releases the device, keeps the push token and leaves `pragma_foreign_key_check` empty; attributing an unregistered device, an unknown member, or a blank id all fail |
| `sqlite_handshake.rs` | the whole rung through the real two-phase pair — a code issued for Liz produces a device that is Liz's and nobody else's; an ordinary code produces an unattributed device; **a pairing body carrying `profile_id: "jerry"` still pairs as Liz**; re-pairing with an ordinary code releases; deleting a member with an outstanding code succeeds and degrades the code |
| `tests/device_profile_rung_is_not_wired_yet.rs` | nothing in `main.rs` or `routes.rs` reaches the rung, and every `paired_device_profile` in `routes.rs` is still `None` |

The handshake tests go through `init_handshake` + `verify_handshake` with a client-computed MAC
rather than writing `devices.profile_id` directly, deliberately. `ProfileScope::Owner` was inert for
a whole phase because every fixture that produced an owned row set the column by hand and no
production path did: **a test whose fixture production cannot produce tests a system that does not
exist.**

### The plan

- **Unit** — `ProfileScope` read/write matrix: `Owner(a)` cannot see `Owner(b)`; both see
  `Household`; `Guest` sees neither.
- **Regression** — a test asserting no production call site passes a "everything" scope, in the
  spirit of `every_settings_field_is_dispositioned`: grep-based, build-breaking.
- **Integration** — two profiles, two sessions, a memory written in each; assert isolation through
  the REST API, not just the repository.
- **End to end** — pair a phone as member A, ask it to recall something only member B told the pond,
  assert refusal plus one `Auth` audit event with `ok = false`.
- **Migration** — restore a pre-`0037` database, confirm every legacy memory resolves as
  `Household` and no row is attributed to the primary member.
