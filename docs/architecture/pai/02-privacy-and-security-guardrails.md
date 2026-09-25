# PAI-2 — Privacy and security guardrails

Requirement: *privacy and security guardrails to minimise data and secret exposure.* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: [PAI-1](./01-identity-and-profile-boundaries.md) — a guardrail needs a subject.

Verified against code 2026-08-03.

---

## 1. What is true today

### 1.1 What is already strong, and must not regress

**Pairing and tokens.** Two-phase HMAC handshake where the six-digit code never crosses the wire
(`security/ports/handshake.rs:1-161`, `pond-infra/src/sqlite_handshake.rs`). Only SHA-256 hashes of
codes and tokens are persisted (`:5-12,164-165`); tokens are 32 `OsRng` bytes (`:96-98`);
comparisons are constant-time (`ConstantTimeEq` for the code hash, `hmac::Mac::verify_slice` in
`verify_mac` for the MAC); TTLs are pairing 10 min, challenge 60 s, session 24 h, refresh 30 d. The
blanket loopback bypass was removed in #94 and now requires `POND_DEV_ALLOW_LOOPBACK=1`.

I originally wrote "five failed attempts lock out". **That is wrong — there is no lockout, and its
absence is deliberate.** `sqlite_handshake.rs` says so directly: a wrong guess "burns the challenge
… and never touches the operator's pairing code, so there is no remote-triggerable lockout", the
`pairing_codes` table has no attempt counter, and `bad_mac_attempts_do_not_lock_out_pairing_code`
asserts it. Brute force is bounded instead by one-challenge-per-attempt plus a per-IP limiter of 10
verify attempts per 60 s over a 30-per-60 s handshake bucket. That is the better design for a
device on a home LAN — a lockout would hand any guest a denial-of-service against pairing — and the
doc should not have implied a mechanism the authors consciously rejected.

**Egress visibility.** `shared/services/egress.rs` is the best privacy primitive in the codebase: a
process-global request context attributes every outbound call to a session and tool (`:25-53`), and
host classification (`classify_host`) uses a curated 17-entry `KNOWN_PUBLIC_SUFFIXES` allowlist where
loopback is `Internal`, allowlisted hosts are `Public`, and **everything else defaults to
`Sensitive`**. Suffix matching is exact-or-dotted, with a test proving
`notwikipedia.org.evil.com` does not match (`classifies_unknown_hosts_as_sensitive`).

*Re-verified 2026-08-13: the two line ranges cited here had rotted — classification was quoted as
`:126-161` when it lives at `:357-397`, and the suffix test as `:233-237` when it is at `:595-606`.
Both now name the symbol instead, which does not rot. The membership changed the same day and the
count did not: `duckduckgo.com` came off with the last two tools that called it (`giap-knowledge`'s
`instant_answer` and `search_web`'s fallback), and `wolframalpha.com` went on with the
`compute_answer` tool that replaced the first of them. Wolfram is keyed, which is a change of kind
rather than of degree for this list — `finnhub.io`, `gnews.io` and `guardianapis.com` are already
keyed, so the established reading of `Public` here is "a public informational API a built-in tool
calls", not "an anonymous one". What the classification does NOT claim is that the far end cannot
attribute the request: an AppID is an account, so Wolfram can profile a pond's questions in a way
Wikipedia cannot. That belongs in the consent story for the key, not in the egress classifier,
whose job is to say which hosts the pond was built to talk to.*

**Sensitivity classification.** `PrivacySensitivity { Public < Internal < Sensitive < Secret }`
(`security/domain/event.rs:56-67`) is ordered and queryable. The audit MCP server excludes `Secret`
in the store query itself rather than post-filtering (`pond-mcp-server/src/audit.rs:36-38,104-113`),
so row limits count only surfaceable events.

**Retention.** `pond-infra/src/pruning.rs` runs every six hours: event log 30 d, sensor readings
7 d, acknowledged camera events 14 d, 500 messages per session, orphaned face embeddings swept. Face
embeddings are deliberately never auto-expired — "biometric data managed by the user" (`:14-19`).

**Data minimisation already practised.** Memory embeddings are `#[serde(skip)]` and never appear in
JSON. Face recognition retains vectors, never images. FCM pushes are **data-only wake pings** with
no title or body, so content never transits Google (`fcm_push_relay.rs:1-26`). Secret *values* are
never returned by the secrets REST API.

### 1.2 The holes

**The policy layer is inert.**
*(FIXED 2026-08-05 by P1. There are two production `audit` call sites now — the identity-assertion
gate in `routes.rs` and the draft-decision gate — and `is_identity_assertion_proven` /
`is_draft_decision_permitted` are real rules. The mode still defaults to `audit`, so nothing is
blocked yet; that is P8's flip. The paragraph is kept because it is the baseline the phase list was
written against.)*

`SecurityPolicy::allow` returned `Ok(true)` unconditionally in both
implementations (`security/services/policy.rs:27-28`, `pond-infra/src/sqlite_security_policy.rs:62-64`).
Every `.audit()` call site in the repository is inside a `#[cfg(test)]` module — `policy.rs:76,79`
and `sqlite_security_policy.rs:125,151,182,200`. The file's own doc comment says so
(`sqlite_security_policy.rs:11-16`).

**API keys are in the settings table and returned over HTTP to anyone.**
*(FIXED 2026-08-05 by P2. All four fields are off `Settings` and in `SecretRepository`, and
`no_settings_field_is_secret_shaped` fails the build if anyone puts one back.)*

`api_key_guardian`,
`api_key_gnews`, `api_key_finnhub`, `api_key_coingecko` and `searxng_url` are `Option<String>`
fields on `Settings` carrying only `#[serde(default)]` — no `skip_serializing`. `GET /settings` does
`serde_json::to_value(settings)`, so it returns every one of them in plaintext. Meanwhile a real
`SecretRepository` port exists (`security/ports/secret.rs`) whose doc says "values are NEVER
returned through the REST API".

I first wrote that this exposed the keys "to any authenticated client". **That was too generous.
Re-checked 2026-08-04: no authentication is required at all.** See below.

**The auth allowlist matches on path only, so several "protected" routes are public.**
*(FIXED 2026-08-05 by P0. Kept in full, because the shape of the mistake is the point and because
the table below is the reproduction record. `is_public_route` now takes a `&Method` and matches a
`PUBLIC_ROUTES: &[(Method, &str)]` table segment-wise; three compile-time guards fail the build if
the table and the router drift. Everything in the table below now requires a token. `PUT /settings`,
`POST /profiles` and `PATCH /profiles/{id}` stayed public for onboarding until P7 landed later the
same day; each entry now carries an `Exposure` class and those three answer a caller with no token
only while onboarding is incomplete. There are four compile-time guards now, not three.)*

`is_public_route` (`pond-api/src/middleware/mod.rs`) received just `path.path()` from
`auth_middleware` — never the method — while its entries were written as though method-scoped:

| Entry | Comment says | Actually public |
|---|---|---|
| `path == "/settings"` | "PUT /settings is public so onboarding steps can save before completion" | **`GET /settings` too — every API key, no token** |
| `"/profiles"` | "POST — create profile during onboarding" | `GET /profiles` (enumerate the household) |
| `path.starts_with("/profiles/")` | "PATCH /profiles/:id — update profile preferences during onboarding" | `GET /profiles/{id}` and **`DELETE /profiles/{id}`** |

The `protected_routes` label in `routes.rs` is cosmetic: `public_routes.merge(protected_routes)`
produces one router, and the single `auth_middleware` layer decides purely on the path string. So
`delete_profile`, registered in the protected group, is reachable unauthenticated by anything that
can open a socket to the pond.

This is the most serious thing this workstream found, it is a live defect rather than a missing
feature, and it should be fixed ahead of the rest of the phase list.

**There is no keyring.**
*(Partly FIXED 2026-08-05. There is still no OS keyring — that was the cheaper-and-clearer call
recorded in 3.2 — but the file has been renamed `pond-infra/src/file_secret_repository.rs` so its
name is honest, and P4 replaced the plaintext JSON with an XChaCha20-Poly1305 envelope under a
keyfile. Grep for `FileSecretRepository::new`; the line number below has rotted.)*

Despite the filename, `pond-infra/src/keyring_secret_repository.rs`
contained only `FileSecretRepository`: plaintext JSON at `<data_dir>/secrets.json`, chmod 0600, with
environment variables taking precedence (`:51-58`). That is what production wired
(`pond-server/src/main.rs:2353`). The `keyring` crate in the root `Cargo.toml` is a Goose submodule
mirror entry, not a GIAP dependency.

> **Half corrected, 2026-08-05 (P4).** The store is no longer plaintext — see 3.4. The filename is
> still a lie and there is still no keyring; renaming the module is P2's job. The env-var precedence
> is unchanged and deliberate. The `file:line` anchors above have rotted and are kept only as a
> record of what was read on 2026-08-03; grep for `FileSecretRepository`, not for a line number.

**There is no redaction.** Grep for `redact|PII|scrub|anonymi` over `crates/` returns only comments
plus `pond-infra/src/push_token_log.rs`, which shortens push tokens for log lines. Nothing inspects
memory content, chat text, or event attributes for personal data before storage.

**There is no encryption at rest.** Both SQLite databases and `secrets.json` are plaintext; the only
protection is the 0600 file mode.

> **Superseded in part, 2026-08-05 (P4).** `secrets.json` is now an
> XChaCha20-Poly1305 envelope under `<data_dir>/secrets/master.key`. Both SQLite
> databases are still plaintext and remain so — full-database encryption is the
> recorded deferral in 3.4, not an oversight. The 0600 mode was also being
> applied *after* the write, so the file existed world-readable for the width of
> a `chmod`; `secret_crypto::write_private` creates the temporary with mode 0600
> and renames instead.

**Onboarding left auth holes open permanently — CLOSED by P7, 2026-08-05.** `PUT /settings`,
`POST /profiles` and `PATCH /profiles/{id}` were on the public allowlist and stayed there after
onboarding completed, so an unauthenticated caller anywhere on the LAN could rewrite settings and
edit any household member's preferences on a pond set up months earlier. Every allowlist entry now
carries an `Exposure` class and those three close the moment onboarding completes. Recorded here
because the citation this paragraph carried — `middleware/mod.rs:165-201` — was already stale when
P7 read it: those lines are `dev_allow_loopback`, not the table. Grep for `PUBLIC_ROUTES`, never for
a line number.

---

## 2. The gap

GIAP is excellent at *observing* privacy-relevant events and poor at *preventing* them. It knows
which host a tool called and classifies it `Sensitive`; it cannot stop the call. It has an eight-scope
authorisation model that authorises everything. It writes API keys into the same table it serves to
clients. Requirement 8 asks for guardrails; today there are gauges.

---

## 3. Design

### 3.1 Make the policy real, but land it in audit mode first

`SecurityPolicy` becomes a deny-by-default matrix over the eight existing scopes ×
`PrincipalKind` × profile. A new setting governs the transition:

```
security_policy_mode = "off" | "audit" | "enforce"     # default "audit"
```

- `off` — today's behaviour, for debugging.
- `audit` — every decision is evaluated and **logged**, but a deny does not block. This is how the
  matrix gets validated against real households before it can lock anyone out of their own pond.
- `enforce` — denies bite.

Landing in `audit` is the whole point. A rules matrix written from first principles will be wrong in
ways only real traffic reveals, and an authorisation regression in a home assistant looks like the
lights not turning on.

`audit()` gets its first production call sites here, alongside PAI-1's cross-profile check.

### 3.2 Secret hygiene

1. `api_key_*` migrate from `Settings` to `SecretRepository`. `init_news_deps` and friends
   (`giap_registration.rs:135-155`) already take a settings repo; they take a secret repo instead.
2. `GET /settings` must not be able to regress. Add a build-breaking guard test in the same spirit
   as `every_settings_field_is_dispositioned` (`settings.rs:1630-1795`): **no serialized `Settings`
   key may match `*_key`, `*_token`, `*_secret`, `*_password`.** A guard test is the right tool here
   because the failure mode is a future field added by someone who has not read this document.
3. `FileSecretRepository` gains an encrypted backend (below). The file keeps its name honest — either
   implement the keyring or rename the module. Rename is cheaper and clearer.

### 3.3 A redactor with an explicit boundary

New port `security/ports/redactor.rs`:

```rust
pub trait Redactor: Send + Sync {
    /// Returns the text with detected personal data replaced, plus what was found.
    fn redact(&self, text: &str, level: RedactionLevel) -> Redacted;
}
```

Deterministic, rule-based adapter first (e-mail addresses, phone numbers, card and IBAN shapes,
API-key shapes, postal addresses). No model in the loop — a redactor that costs a 20 tok/s inference
call will be disabled by the first user who notices.

Applied at exactly three chokepoints:

1. Before a `MemoryFragment` is written.
2. Before event `attributes` reach the event log.
3. Before any body leaves the pond (PAI-8 connectors, webhooks).

*(Updated 2026-08-05 by P3. 1 and 2 are landed, as port decorators — and 2 covers three separate
`EventLog` `Arc`s, not one: the shared binding, the egress sink that reads it, and the
`SqliteSecurityPolicy` audit sink, which is constructed independently. **3 has no call site to wire.**
No connector exists, the live webhook arm sends no body, and the body-carrying executor is never
constructed. It moves to P6b. See the P3 entry in section 5.)*

**Explicit non-goal, stated so nobody 'fixes' it later: the redactor is not applied to the model's
own prompt.** Redacting the assistant's view of your life is what makes it useless. The privacy
property GIAP offers is *the model runs on your hardware*, not *the model is blindfolded*. What
redaction protects is the durable stores and anything that leaves.

### 3.4 Encryption at rest, honestly scoped

Full-database SQLCipher is a real lift: a different SQLite build, key custody, a Jetson cross-build,
and `pond_system.db` is read on every turn and every session listing.

**v1 encrypts what actually hurts**: `secrets.json` and connector tokens, with XChaCha20-Poly1305
under a keyfile at `<data_dir>/secrets/master.key` (0600, in a 0700 directory), generated on first
run. This is a small, testable change that removes the "your Gmail refresh token is a plaintext
string on a home server" problem, which is the one PAI-8 creates.

The SQLCipher path for the full database is designed here and **deferred with its cost written
down**, rather than promised. Re-open it when someone is prepared to own the cross-build.

**LANDED 2026-08-05.** What shipped, and the parts the design above did not answer:

- **XChaCha20-Poly1305, not ChaCha20-Poly1305.** The 192-bit extended nonce is drawn at random for
  every write, so there is no counter to persist and no collision argument to make. Same crate
  (`chacha20poly1305 0.10`, already in `Cargo.lock` via `nostr` under the goose submodule), same
  default features, no extra build cost on a Jetson.
- **Coverage is bounded by P2.** Connector tokens were already in `SecretRepository` — the OAuth
  callback writes `access_token`/`refresh_token` through `repo.set` — so P4 covers them today. The
  four `api_key_*` fields are still rows in the plaintext `settings` table in `pond_system.db` and
  stay plaintext until P2 moves them. P4 did not need P2 to land; P2 decides how much P4 protects.
- **Threat model, said plainly.** This protects a *copy of the file*: a backup set, an rsync that
  excludes the key, a support bundle, a database handed to someone for debugging. It does not
  protect a running pond, which must decrypt to hand a token to an extension subprocess. And in the
  default layout it does not protect a stolen Jetson either, because the key is in the same data
  directory as the ciphertext — `POND_SECRET_KEY_FILE` exists so an operator can separate them, and
  that is the only configuration in which this beats someone walking off with the board. Anything
  in the UI that implies more than this is wrong.
- **Key generation.** Eager, at repository construction, not lazily on first write. A fresh pond
  therefore has a key to back up from day one, and the first `set` cannot fail for a reason
  unrelated to the secret being set. The key is stored base64-encoded with a trailing newline so a
  human can copy it into a password manager — that is the only backup path this design offers, and
  a key nobody can read out is a key nobody backs up.
- **Migration.** In place, at the same path. Order is: load-or-create the key and `fsync` it, then
  encrypt into `secrets.json.tmp`, `fsync`, `rename`, `fsync` the directory. An interruption at any
  point leaves either the intact plaintext file or the complete ciphertext — never a half-written
  store, and never ciphertext whose key was not durable first. A plaintext file that does not parse
  is now an **error**, where the old code called `unwrap_or_default()` and turned a corrupt store
  into an empty one on the next write.
- **The ordering has a test, which the plan for this phase said it would not.** That was worth
  arguing with: key-before-ciphertext is the single property standing between an interruption and
  permanently unopenable secrets, and "documented and implemented, not tested" is how it quietly
  gets reversed by a later refactor. `the_key_is_durable_before_any_ciphertext_is_written` makes the
  ordering observable by forcing the ciphertext write to fail — it pre-creates `secrets.json.tmp`
  as a *directory*, which defeats the `create_new(true)` in `write_private` and stands in for the
  ENOSPC or EIO that would cause this in the field — then asserts the key file is already on disk,
  already loadable, and the plaintext store is untouched. Reversing the order was tried: it is the
  **only** test in the suite that fails, which is exactly why it had to exist.
- **`FileSecretRepository` has a hand-written `Debug` that redacts.** Deriving it would have printed
  the master key and every secret value; the tests below call `expect_err`, which formats the struct,
  so the derived version would have put the key straight into test output the first time a
  construction unexpectedly succeeded. Not a hypothetical — it happened while writing the mutation
  test for the locked-store guard.
- **Downgrading past this commit destroys the store, and nothing can stop it.** A pre-P4
  `FileSecretRepository::new` parses `secrets.json` with `serde_json::from_str(...).unwrap_or_default()`.
  Handed an envelope it does not understand, it yields an **empty map, with no error and no log
  line** — and the next `set` writes a plaintext file over the ciphertext. That is the same
  fail-open this phase removed, running in the opposite direction, and the old binary is the one
  doing it, so no code here can prevent it. If you must roll back below this commit, copy
  `secrets.json` and `secrets/master.key` somewhere else first. It is also the reason the migration
  forward is automatic rather than prompted: leaving the plaintext file in place to be polite would
  mean leaving a real Spotify refresh token readable on disk indefinitely, which is the problem.
- **Key loss: the secrets are gone.** There is no escrow, no recovery code, nothing to ask support
  for. The store refuses to open (`SecretStoreLocked`) rather than starting empty, precisely so that
  a temporarily misplaced key does not become permanent loss on the next write. It surfaces in three
  places: two `ERROR` lines at startup naming the key path and the remediation, a `503` from every
  `/secrets` route (so the Extensions view in `pond-desktop` shows the store as unavailable rather
  than as empty), and a `giap.sh doctor` FAIL that spells out the recovery — move `secrets.json`
  aside, restart, re-enter keys, re-authorise connectors.
- **Jetson specifics.** The rootfs is usually on removable media, which makes "stolen device" mean
  "pulled the SD card" and makes the key-beside-ciphertext default worth stating out loud. Power is
  removed by whoever is nearest the socket, which is why the directory `fsync` after the rename is
  there. There is no fTPM or secure element exposed on an Orin Nano devkit, so there is no hardware
  sealing to fall back on. Write volume is unchanged from the plaintext store (whole file per set),
  so flash wear is not a new concern.

### 3.5 Egress becomes enforcement

`record_egress` is promoted from observability to a gate:

```
network_mode = "open" | "allowlist" | "offline"        # default "open" for compatibility
```

`allowlist` refuses hosts that classify as `Sensitive` — reusing `KNOWN_PUBLIC_SUFFIXES` and the
existing fail-Sensitive default, which is exactly the right polarity for this. `offline` refuses
everything but loopback, which makes "prove it is not phoning home" a one-setting demonstration
rather than a packet capture.

Every new outbound adapter uses `egress::begin` / `EgressCall::finish` (P5) rather than copying
`traced_send` from `pond-adapters-weather`. Verified 2026-08-05: `traced_send` is at lines 12-31 of
`pond-adapters-weather/src/lib.rs`, not 15-30 -- cite the symbol, not the range.

**The guard as originally specified is not implementable, and the correction matters.** "Every crate
with a `reqwest` dependency references `record_egress`" fails for 8 of the 11 such crates on the day
it lands, and most of what it flags is a health probe against a model server on 127.0.0.1. A guard
that reports loopback as egress gets switched off within a week. `crates/pond-core/tests/egress_guard.rs`
therefore forces a three-way classification of every HTTP-sending FILE -- `EGRESS_TRACKED` (checked
by symbol), `LOOPBACK_ONLY` (checked by reading every URL literal in the file, so the claim is
mechanical rather than a comment), `UNGATED_SENDERS` (enumerated, capped, shrink-only) -- and keeps
the crate-level rule in the only form that holds: every `reqwest` crate owns at least one classified
file, which is what catches a sender using `Client::execute` or `reqwest::blocking`.

**`network_mode = "offline"` is not yet a complete claim.** ~~Six real-egress files are still
ungated (HF/GitHub model downloads, OAuth token refresh, Spotify, the vision-encoder download, the
MCP connectivity probe).~~ *Superseded 2026-08-06 by P6a: five of the six are gated and
`UNGATED_SENDERS` is down to one entry, `pond-api/src/routes.rs`.* They are enumerated in
`UNGATED_SENDERS`; this is not done until that list is empty, and the cap is what stops it becoming
a parking lot.

**A file-level guard is necessary and not sufficient, and P6a is where that stopped being a
footnote.** `egress_tracked_files_reach_the_tracker` checks for ONE tracker symbol per FILE, so a
file with several senders goes green on the first one gated. Three files in the list have more than
one: `pond-hf-cache/src/lib.rs` (two redirect loops), `pond-server/src/model_download.rs` (three),
and `pond-api/src/routes.rs` (nine, still ungated). Those need a BEHAVIOURAL test per site, and the
mutation that proves it is not paranoia is in P6a's entry below — gating one of the HF cache's two
hops leaves the file-level guard green while the other still resolves DNS to a third-party host.

**A sender is not always a `reqwest` call.** The guard finds senders by looking for `reqwest`, and
P6a found one it cannot see: `download_and_extract_ort` in `pond-server/src/main.rs` shells out to
`curl` for a ~100 MB ONNX Runtime tarball from github.com. It is the only subprocess sender in the
tree (`Command::new("curl")` matches there and nowhere else), so the hole was one call — but it was
the largest single outbound transfer the pond makes, and no amount of `reqwest` scanning would ever
have reported it. When adding a sender, ask what the guard can SEE, not what it lists.

### 3.6 The outbound-action gate

> **This gate no longer exists, 2026-09-10.** `giap-draft` was deleted with five other tool groups
> to cut the prompt's tool payload, and it was the pond's only staging-and-confirmation surface. The
> paragraph below describes what was there; read it as history, not as a mechanism to grep for.
>
> What survives: the rule it carried is now prose on `set_device_state`'s own description — unlocking
> a door or disarming an alarm needs the user's explicit go-ahead in the same message. That is a
> prompt-surface control, weaker than a tool that could not execute without a second call. The event
> log still records every action; nothing reads it back through a tool now that `giap-audit` is gone
> either. **A replacement gate is unbuilt, and PAI-2 P-outbound should be treated as regressed rather
> than satisfied until one exists.**

`giap-draft` was unconditionally registered as a safety extension and models saved / listed /
approved / rejected. Every side-effecting action from a connector — send, post, publish, delete —
routed through it, scoped to the acting profile. This was reuse of a mechanism that existed and was
already trusted, not a new approval system.

> **Two corrections, 2026-08-05.** The `draft.rs:97,167,200,248` citation above had rotted — two of
> the four line numbers were wrong when P1 went looking. Grep for the symbol, as this section now
> does. And "scoped to the acting profile" was aspirational rather than descriptive: until P1 there
> was no acting profile on a draft at all. `drafts` had no owner column, and the `session_id` it did
> have was a value the *model* filled in, defaulting to the literal `"default"`. See the P1 entry.

### 3.7 Close the onboarding holes

`PUT /settings`, `POST /profiles` and `PATCH /profiles/{id}` leave the public allowlist once
onboarding is complete. `middleware/onboarding_guard.rs` already knows that state.

**The design as written above is incomplete, and the missing piece is `POST /onboard/reset`.** Reset
is public and stays reachable after onboarding, because it is the recovery lever for a misconfigured
pond. It also *undoes* the closure by design: after a reset the pond is not onboarded, so the three
routes above are open again. That leaves two failure modes, and a design has to get past both:

- If the closure is a **latch** ("this pond has been onboarded before"), reset is a one-way door.
  The reset succeeds, the pond drops back to the wizard, and the wizard's three writes stay shut
  forever. The only repair is reflashing the device.
- If reset stays **unconditionally public**, the closure is decorative. An anonymous caller resets
  the pond and walks in through the holes the reset reopened.

So: the gate reads onboarding state **live**, on every state-dependent request, with no cache and no
`Settings` flag — and reset itself becomes public-while-onboarding plus **loopback-only afterwards**.
Loopback is not a new trust boundary here: `handshake_pairing_code` and
`handshake_issue_pairing_code` already refuse a non-loopback peer inside the handler, so a pond that
has lost every token can only be re-paired from the host already, and `is_identity_assertion_proven`
states the same rule — whoever is at the console already has the box. Recovery gains no requirement
it did not have.

One consequence for the guards: the allowlist becomes state-dependent, and a compile-time table
check cannot express "public only while onboarding is incomplete". The guards therefore ask the only
question a static check can answer honestly — *could this route ever answer without a token, in some
state* — which is the worst case, and a fourth guard pins the classification itself so a route
cannot change class quietly.

---

## 4. Phases

- **P0 — LANDED 2026-08-05.** `is_public_route` takes a `&Method`, and the allowlist is a
  `PUBLIC_ROUTES: &[(Method, &str)]` table whose `{brace}` segments match exactly one path segment.
  The four reproduced leaks are closed: `GET /settings` and `GET /profiles` now 401, while the
  `PUT /settings` and `POST /profiles` that onboarding needs stay open — same paths, different
  answers, which is the whole point. `DELETE /profiles/{id}` was public because of a
  `starts_with("/profiles/")` prefix test that also covered `GET` and every other method on any
  sub-path; segment-wise matching ends that.

  **Three build-breaking guards, because the allowlist and the router are two lists that must agree
  and nothing structurally forced them to.** All three parse `routes.rs` through `include_str!`, so
  they run at compile time with no runtime file IO and cannot drift:
  `every_protected_route_requires_a_token` (P0's stated acceptance test),
  `public_router_and_allowlist_agree` (drift in *either* direction — a public route missing from the
  allowlist 401s during onboarding, an allowlist entry with no route is an exemption that outlives
  its reason), and `the_pai2_p0_leaks_are_closed`, which states the four leaks as the HTTP requests
  that leaked.

  **Two exceptions are real** and are listed individually rather than skipped by prefix, so a third
  `/oauth/*` route cannot join them silently: `GET /oauth/callback` (the browser arrives from the
  provider with no token; the PKCE state nonce authenticates it) and `POST /oauth/refresh` (checked
  against `internal_extension_token` inside the handler). Both live in the *protected* router, which
  is why "every route in `protected_routes` requires a token" could not be a blanket assertion — the
  router's split is about **onboarding**, and `is_public_route` is about **auth**. Two different
  axes that read like one.

  **The guards were mutation-tested rather than trusted.** Re-adding `(Method::GET, "/settings")` to
  the table fails all three, with the offending route named. Worth the two minutes: this programme
  has three recorded vacuous-test incidents, and a guard that cannot fail is worse than none because
  it reads as coverage.

  **The fix exposed a test that had been passing for the wrong reason.**
  `settings_is_blocked_before_onboarding` sent no token and asserted 403. It passed only *because*
  `GET /settings` was on the public allowlist: the request went straight through auth and was
  stopped by the onboarding guard. Once auth could refuse it, the assertion saw 401 — the correct
  answer — and the test was the thing that had to change. Every sibling in that file already carried
  a bearer token for exactly this reason; this one never needed one, because it could not reach auth
  to be stopped by it. **A test asserting the right status for the wrong reason is indistinguishable
  from a correct one until the reason moves**, which is the same lesson as the unreachable
  `ProfileScope::Owner` fixtures, arriving from the opposite direction.

  Verified live: `scripts/live-test.sh --ui` passes end to end — all five probed routes 401 with no
  token and the bypass off, 37 API checks, 4/4 live UI. `pond-api` 109 lib tests (105 + 4) and 137
  across all 17 integration targets (135 + 2: `GET /settings` unauthorised over real HTTP with
  onboarding complete so a 403 cannot mask it, and `PUT /settings` still open — if that one ever
  401s, onboarding deadlocks on a pond nobody can finish setting up). `pond-core` 736. fmt and
  clippy clean.

  Original phase text, for the record:

- **P0 (as designed) — do this first, ahead of any design work.** Make the auth allowlist method-aware. Every
  entry in `is_public_route` is written as though it were method-scoped and none of them are. Pass
  the `Method` alongside the path, enumerate `(Method, path)` pairs rather than paths, and add a
  test asserting that every route in `protected_routes` requires a token — the
  `public_routes.merge(protected_routes)` split gives a false sense of safety otherwise. This is a
  small, self-contained fix and it should not wait behind the policy-mode work.

  **Reproduced against a running server, 2026-08-04.** Not inferred from reading the allowlist —
  driven with `curl` against `pond-server serve`, no `Authorization` header, and with
  `POND_DEV_ALLOW_LOOPBACK` unset so the loopback bypass was off:

  | Request | Expected | Actual |
  |---|---|---|
  | `GET /api/v1/settings` | 401 | **200, with `api_key_gnews` and `api_key_finnhub` in plaintext** |
  | `PUT /api/v1/settings` | 200 (deliberately public for onboarding) | 200 |
  | `GET /api/v1/profiles` | 401 | **200, the whole household listed** |
  | `DELETE /api/v1/profiles/{id}` | 401 | **204, member deleted** |
  | `GET /api/v1/sessions` | 401 | 401 |
  | `GET /api/v1/devices` | 401 | 401 |
  | `GET /api/v1/memory` | 401 | 401 |

  The last three matter as much as the first four: the auth layer *does* work, so this is not a
  missing middleware. It is precisely the allowlist entries being matched on path alone.

  The `PUT` row is what turns a read leak into a full chain — an unauthenticated caller on the LAN
  can write a key through the onboarding hole and read it back through the path-matching hole. Both
  halves were exercised in that order and both succeeded.

  One qualifier, because it changes how urgent this looks on a fresh install: `GET /settings` leaks
  no key *value* until a key is actually configured. The exposure is real for any pond whose owner
  has set one up, and invisible before that.

  **The reason I first gave for that qualifier was wrong, and it contradicted section 1.2 of this
  same document.** I wrote that the key fields carry `skip_serializing_if = "Option::is_none"`.
  They do not — verified 2026-08-05, all four `api_key_*` fields carry only `#[serde(default)]`,
  exactly as 1.2 says. An unset key is therefore *serialized as `null`* rather than omitted, which
  happens to leak no value and is not the mechanism I claimed. Worth recording because the two
  halves of one document disagreed and the wrong half was the one attached to the reassuring
  conclusion. **P0 put a bearer token in front of this route; the keys are still on the struct and
  still serialized into the body. That is P2, and it is untouched.**
- **P1 — LANDED 2026-08-05, without the matrix.** `security_policy_mode` with `audit` default, and
  two production `allow`/`audit` call sites (shared with PAI-1 P4).

  **The scope × principal matrix was deliberately not built**, and the header says so because every
  other phase's header is scannable and this one's omission looked like an oversight for a day.
  Eight scopes crossed with three `PrincipalKind`s is twenty-four cells, and every one of them has
  to be `allow`: each kind legitimately needs each scope for something that exists in the code
  today, and denying `Internal` anything breaks background work silently rather than returning an
  error to anybody. Twenty-four allows is not a security control, it is a table shaped like one.
  The axis that actually discriminates is whether a caller has **proved** the identity it claims —
  which is what both call sites key on instead.

  **Mode and first call site LANDED 2026-08-05** (`PolicyMode`, `PolicyDecision`,
  `is_identity_assertion_proven`, `Principal.proven_profile_id`, and
  `evaluate_identity_assertion` in `routes.rs`).

  **Second call site LANDED 2026-08-05 — the draft-decision gate.** This is the one Jerry decided
  belonged here rather than in a standalone handler patch, so that `approve_draft`'s missing
  ownership check and the layer meant to express it would land together, in `audit` first.

  What shipped, and where it differs from the sketch above:

  - **Migration 0038** adds `drafts.profile_id` and `drafts.identification_source` (the same
    vocabulary `sessions.identification_source` uses, so a decision taken on a 0.62 face match stays
    distinguishable from one taken on a paired token), plus an index on `(profile_id, status)` and
    one on `engine_session_map(engine_session_id)`, which had none.
  - **Existing rows are stated, not left implicit.** Every draft that already exists is pending, in
    the `"default"` bucket, with no owner. Leaving them pending would hand the first person to say
    "approve" after the upgrade the right to run somebody else's staged shell command — the exact
    outcome this phase exists to prevent. They are set to `expired`, which finally gives
    `DraftStatus::Expired` a producer. Owned-by-nobody-and-approvable-by-anybody was the defensible
    alternative and it is rejected in writing, in the migration. The cost is real and small: a user
    mid-confirmation across a restart is told the draft expired and asks again. A `BEFORE DELETE ON
    profiles` trigger expires and then releases a departed member's pending drafts, in that order —
    releasing first would hide the rows from the expiry.
  - **The rule is `is_draft_decision_permitted`**, in `security/ports/policy.rs` next to
    `is_identity_assertion_proven` so the two are read together. Unresolvable caller: refuse.
    `Guest`: refuse, behind the tool-group denylist rather than instead of it — PAI-1 P5 shows that
    gate can go inert for a day without anyone noticing. `Owner(id)` decides only its own.
    `Household` decides anything, because `identity_resolution::resolve` only yields `Household` on
    a **one-member** pond and refusing there would break the assistant for its only user while
    protecting nobody. An **unowned** draft falls back to the session that staged it, which is
    weaker than an owner match and is deliberately not an outright refusal: refusing every unowned
    draft would make the feature permanently unusable on any pond whose speakers are not identified,
    which is a regression dressed as a control.
  - **The caller is read from the MCP request `_meta`, not from a global and not from the model.**
    This is the part the design did not anticipate and it corrects a claim PAI-1 recorded twice.
    Goose stamps `agent-session-id` into every `CallToolRequest`'s `Meta`; rmcp serialises it as the
    wire `_meta` and swaps it into the `RequestContext.meta` the tool handler receives. So a
    process-global builtin **does** have a race-free per-call session channel, and
    `set_current_session_id` — a `RwLock<String>` that `Semaphore::new(4)` chat streams race — was
    never the only option. Using that global here would have traded an authorisation hole for a
    misattribution bug. `crates/pond-mcp-server/src/session_meta.rs` is the reader, and the other
    process-global builtins (`giap-memory`, `giap-toolkit`) can adopt it.
  - **`save_draft` and `list_drafts` were fixed on the way, and had to be.** The `session_id` tool
    *parameter* is model-supplied and defaulted to `"default"`, so every draft on every pond sat in
    one bucket and `list_drafts` read everybody's out of it — which is what made an id enumerable
    and the approve hole exploitable. Both now scope by the engine session; the parameter survives
    in the schema as advisory and loses to it. `save_draft` stamps the owner from the resolved
    scope, so the column is populated by production code rather than only by fixtures.
  - **`reject_draft` had no guard at all** — not existence, not status — so it would flip an
    already-approved draft to rejected long after the action was authorised. `approve` and `reject`
    now share one `decide()` path, so the ownership check cannot be added to one and forgotten on
    the other.
  - **Audit-mode telemetry:** every decision records `draft_approve:allow|would_deny|deny` (and the
    `reject` equivalents) through `SecurityPolicy::audit` under a new `scopes::DRAFT`, and a
    `would_deny` also emits `kind = "policy_would_deny"` at WARN.

    *Superseded 2026-08-06 (P8a, landed).* The action string is now a plain `draft_approve` /
    `draft_reject` and the verdict is an event **attribute**. Read the P8a entry below before
    trusting any `:verdict`-suffixed string named anywhere in this document.

    *Corrected 2026-08-06 (P8a recon).* This sentence used to end "that is the evidence P8 needs
    before the default flips to `enforce`". It is not, yet — it is the raw material for that
    evidence and nothing refines it. Three facts, each checked against code:
    **(a) nothing counts.** `rg would_deny` over `crates/`, `pond-desktop/src` and `scripts/`
    returns only the two call sites, their doc comments and their tests. There is no counter, no
    aggregate and no report. **(b) The verdict is not a field.** It is smuggled into the audit
    `action` *string* — `routes.rs` writes `format!("identify_session:{}", decision.verdict())`,
    `draft.rs` writes `format!("draft_{verb}:{}", ...)`, and `RepoDraftAuthority::audit` re-wraps
    that as `format!("{action} session={engine_session_id}")`, so the stored attribute is literally
    `"draft_approve:would_deny session=abc"`. Counting would-denies today means substring-parsing a
    composed string. **(c) It is not operator-reachable as a count and it expires.**
    `GET /api/v1/activity?category=auth` returns raw rows; `GET /api/v1/activity/summary` aggregates
    by `EventCategory` only (`routes.rs :: activity_summary`), so it cannot separate `allow` from
    `would_deny`. `pruning.rs` caps Sensitive events at `events_sensitive_days: 7`, and
    `DELETE /api/v1/activity` purges with `max_sensitivity: None` *deliberately* — so the user's own
    "clear my activity" button erases the enforcement evidence. See P8a.

    *Status of those three facts after P8a landed, 2026-08-06.* **(a) is fixed** — `POLICY_COUNTERS`
    counts, and `GET /api/v1/security/policy-report` aggregates. **(b) is fixed** — `verdict` is an
    attribute on the stored event and the action string is a plain verb. **(c) is fixed as a
    reachability problem and deliberately NOT fixed as a durability one**: the report exists and an
    operator can read it, but the events half is still capped at seven days and still erasable by
    `DELETE /api/v1/activity`. That is why the report carries a second, process-lifetime block
    instead of one merged number. Making the audit trail itself un-erasable is a retention decision
    that belongs with `events_sensitive_days`, not with a read surface.
  - **Deviation from the plan:** no `PrincipalKind::AgentTurn`. A tool call has no HTTP principal,
    so the audit line uses `Principal::internal()` and carries the engine session in the action
    string. Inventing a principal kind whose `proven_profile_id` is still `None` would have looked
    like proof and been none.

  **Mutation-tested, not assumed.** Restoring the hole (`ProfileScope::Owner(_) => Ok(())`) fails
  three tests at three layers: the rule test (`Ok(())` vs
  `Err("draft belongs to a different household member")`), the tool test (`Approved` vs `Pending`
  under enforce), and the SQLite chain test. Flipping the comparison to `!=` fails it the other way
  — `the_owner_may_still_approve_their_own_draft_under_enforce` reports the deny text — which is the
  check that separates "the rule discriminates" from "the rule refuses everything".

  **`crates/pond-infra/tests/draft_ownership_chain.rs` exists because of this programme's recorded
  vacuity failure.** `ProfileScope::Owner` was inert in production for a whole phase while every
  test passed, because the fixtures set the owner column by hand and no code path did. That test
  builds two members, binds a session through `set_session_identity_if_stronger` (what
  `PUT /sessions/:id/user` calls) and pairs an engine session through `set_engine_session_id` (what
  every turn calls), then asserts an engine session id resolves to a **named** member. If it
  resolved to `Household` or `None`, `save_draft` would stamp NULL forever and every deny test in
  `policy.rs` would still pass.

  Gates: fmt clean; `cargo test -p pond-core -p pond-infra -p pond-mcp-server -p pond-api` green;
  `cargo check -p pond-server -p pond-adapters-goose` clean; `scripts/live-test.sh --ui`. 0038 was
  additionally replayed by hand against a database carrying a pre-existing pending draft, since a
  migration that only works on an empty file works exactly once.
- **P2 — LANDED 2026-08-05.** The four `api_key_*` fields are off `Settings` entirely and live in
  `SecretRepository`, which P4 had already encrypted the hour before — so the migrated values landed
  as ciphertext and never sat in plaintext in between. That ordering was not luck: P4 and P2 both
  wanted `keyring_secret_repository.rs`, and P2 rewriting it wholesale would have silently removed
  the encryption AND destroyed every stored secret while compiling with every test green. P4 owned
  the file's body; P2 only renamed it to `file_secret_repository.rs`.

  `secret_migration::migrate_api_keys_to_secret_repository` copies a configured key into the secret
  store, proves the write landed, and only then clears the settings row — key-first, never
  delete-first, so an interruption strands nothing. It uses `list_keys()` rather than `has()` as the
  already-migrated predicate, because `has()` consults environment variables and would report a
  same-named env var as "already stored", deleting the settings row without ever copying the value.
  `api_key_coingecko` has no reader anywhere in the codebase, and its value is migrated anyway: a
  configured key that vanishes on upgrade is worse than a dead field.

  **The durable part is `no_settings_field_is_secret_shaped`**, which fails the build when anyone
  adds a `*_key`/`*_token`/`*_secret` field to `Settings`, with an error naming the field and
  pointing at `SecretRepository`. Mutation-tested rather than assumed: adding
  `api_key_mutation_probe` fails it by name. An escape hatch, `NOT_ACTUALLY_SECRET`, takes a reason
  — so a false positive is a one-line documented exemption rather than a motive to delete the guard.

  **This did not close the write half — P7 did, later the same day.** When P2 landed, `PUT
  /settings` was still public and unauthenticated, so a LAN caller could write settings on an
  already-onboarded pond; that was the reason P7 landed last. P0 closed the read; P2 removed the
  credentials from what the read returns; P7 closed the write, which now answers a caller with no
  token only while onboarding is running. Read the two entries together: this paragraph described
  the tree as it stood between the two commits, not as it stands now.

  Landed by hand after the implementing agent was killed mid-phase by an account spend limit. Its
  work compiled and was substantially complete; I ran the gates it never reached, mutation-tested
  its guard, reverted six regenerated Playwright screenshots it had picked up incidentally, and
  stamped this entry. Gates: fmt clean, pond-core 753, pond-infra 205, pond-mcp-server 177,
  pond-api 109 lib + 5 settings integration.
- **P3 — LANDED 2026-08-05, at two chokepoints of three.** `Redactor` in
  `security/ports/redactor.rs`; the kinds, the level and every accept/reject rule in
  `security/domain/redaction.rs`; `RuleRedactor` in `pond-infra`. The split is the point: the
  adapter's regexes only *propose* candidates and `candidate_is_real` decides, so replacing the
  engine moves no decision. Luhn, IBAN mod-97 and the real UK inward-letter set are what stop the
  rules eating prose, and each is a pure `&str -> bool` in core with its own false-positive test.
  "B2 3AM" matches every loose postcode regex ever written and is not a postcode, because M is not
  an inward letter.

  Wired as **decorators over the ports**, not as edits at call sites:
  `RedactingMemoryRepository` (level `Secrets`) wraps **all four** `memory_repo` constructions in
  `main.rs`, covering extraction, the `giap-memory` MCP tool, `POST /memories`, consolidation and
  any writer nobody has added yet; `RedactingEventLog` (level `Full`) wraps **both** write-path
  `EventLog` `Arc`s.

  The plan I started from claimed one memory construction and one event log. Both counts were
  wrong, and both errors fail open:

  * `SqliteSecurityPolicy` is handed its own, independently constructed `SqliteEventLog`, several
    dozen lines above the shared `event_log` binding. Wrapping only the shared one would have left
    every row P1 writes — carrying a `token:<client_id>` principal label and a remote address, and
    classified `Sensitive` for exactly that reason — bypassing the redactor, by the one component
    whose entire job is the audit trail.
  * `run_chat`, `run_agent_cmd` and `run_memories_cmd` each build their own `memory_repo`. The
    first two hand it to `build_goose_backend`, which registers `giap-memory`, so a CLI or voice
    session writes memories exactly like the server does; `pond memories add` writes one straight
    from `argv`, which is where a shell-history copy of a credential comes from. `run_server` being
    the only construction was true of `run_server` and false of `main.rs`.

  Wrapping the shared binding also covers egress: `pond_mcp_server::set_egress_sink(event_log.clone())`
  reads it about a hundred lines below, so outbound URLs — the richest source of query-string PII in
  the system — go through the redactor without `record_egress` knowing. The three remaining raw
  `SqliteEventLog` constructions, the ones handed to `init_audit_deps` in `run_server`, `run_chat`
  and `run_agent_cmd`, are deliberately left alone: they are `.into_dyn()` read handles for the
  `giap-audit` extension, not write paths, and redacting a read handle scrubs nothing on the way in
  and double-scrubs on the way out.

  **A source-level guard holds all of that**, because none of it fails to compile:
  `pond-infra/tests/redaction_chokepoints_are_wired.rs` reads `main.rs` with `include_str!` and
  asserts every `SqliteMemoryRepository::new(` is inside a `RedactingMemoryRepository::new(`, every
  non-`.into_dyn()` `SqliteEventLog::new(` is inside a `RedactingEventLog::new(`, and that
  `set_egress_sink` still takes the wrapped `event_log` binding — the last one because **P5 lands
  next and reads that binding**, and a P5 that builds its own log compiles perfectly. It lives in
  `pond-infra` rather than `pond-server` for an unglamorous reason: `ci.yml` does not run
  `cargo test -p pond-server`, so a guard there would never fire on a pull request. It found the
  `run_memories_cmd` write path on its first run.

  **The levels differ and that is a decision, not an oversight.** Memory is read back into the
  model's context, so redacting an email address there is the blindfolding this document's own
  non-goal rejects; a credential is different — it rotates, it is never worth recalling, and its
  presence in a durable store is pure liability. Event attributes are telemetry nobody recalls, so
  they run at `Full`. A finding also **raises** the event's `PrivacySensitivity`, which narrows on
  both axes at once: the audit MCP read path excludes `Secret` at the store, and `pruning.rs` caps
  `Sensitive`/`Secret` at 7 days against 30. A memory whose finding was `Secret` also has its
  embedding cleared, because a vector computed over the secret is a durable derivative of it; the
  relevance backfill re-embeds from the now-redacted `content`, so it is self-healing.

  **The third chokepoint does not exist.** Verified 2026-08-05, not inferred: `rg -ril connector
  crates/` returns nothing — PAI-8 is DESIGNED only. The live webhook arm
  (`schedule_executors.rs`, `TaskKind::Webhook`) does `.post(webhook_url).send()` with **no body at
  all**. The one executor that does send a body, `WebhookTaskExecutor::execute` in
  `pond-infra-scheduler`, is referenced exactly once in the repository — by its own `pub use` — and
  is never constructed. Wiring a redactor into either would have redacted nothing while reading as
  coverage, which is the failure PAI-1 P4 named when it refused to build a matrix of twenty-four
  allows. It lands with **P6b**, alongside PAI-8, and P6b now owns it.

  **Two things found on the way.** Neither webhook path calls `record_egress`, so a webhook fire is
  invisible to the activity API — recorded, not fixed here, because it belongs to P5's
  `reqwest`-implies-`record_egress` guard. (No longer true as of 2026-08-05: P5 routes both webhook
  executors through `egress::begin`/`finish`, so a webhook fire is both gated and recorded.)
  And `memory_extraction.rs` logged every stored fact's raw
  content at INFO, which is the level the on-disk log file keeps; that one **is** fixed here,
  because a second plaintext copy of every memory with none of the store's scoping, none of its
  retention and none of chokepoint 1's redaction is exactly what this phase exists to stop. It never
  reached `pond_logs.db` — the drain gates INFO on the `giap::trace` target and that line used the
  default one — so the file under `<data_dir>/logs` was the whole exposure, which is also the copy
  nobody prunes.

  Deliberately not added: a `redaction_mode` setting. The level is an associated const on each
  decorator, so changing a chokepoint's posture is a one-line reviewable diff rather than a runtime
  knob that has to be classified in `UI_WIRED` or `HEADLESS_BY_DESIGN`. Declared limitation: an
  unprefixed 64-hex-character secret is **not** detected, because it is indistinguishable from a git
  SHA and eating every commit hash out of a developer's memories is the "worse than none" failure
  this phase is built to avoid.

  Mutation-tested, all three halves — the wiring first, because that is where the bypass lives.
  Replacing the audit sink's wrapper with the raw `SqliteEventLog` makes
  `every_event_log_write_sink_goes_through_the_redactor` fail with `main.rs:2593 builds a write-path
  SqliteEventLog outside RedactingEventLog`. Rebinding `set_egress_sink` to a fresh log — the P5
  hazard — fails two guards, one of them naming the rebind. Rules: replacing the inward-letter check
  in `is_uk_postcode` with a plain `is_ascii_alphabetic` makes
  `ordinary_smart_home_prose_is_untouched` fail with
  `mangled: Play B2 3AM by the band when I get home.` and
  `a_postcode_shaped_phrase_in_prose_is_not_a_postcode` fail with `B2 3AM must not read as a
  postcode`. Forwarding: deleting the `count_for_profile` forward makes
  `every_memory_repository_method_is_forwarded` fail by name — that guard exists because 18 of the
  port's 22 methods have default bodies, so a missing forward compiles and silently answers `Ok(0)`
  instead of reaching SQLite. Gates: fmt clean; pond-core 778 + 3 integration, pond-infra 213 + 3
  wiring; `cargo check -p pond-server -p pond-adapters-goose` clean; `scripts/live-test.sh --ui`
  green. That run now carries `section_redaction` in `scripts/live_checks.py`: it POSTs a memory
  containing a credential and a postcode-shaped phrase over real HTTP at the default level with no
  configuration, then reads the row back out of `memory_fragments` and asserts the credential is
  gone, the placeholder is there, and the prose ends byte-for-byte as sent. That is the assertion
  the unit tests cannot make, because they construct the decorator themselves and never ask what
  `run_server` bound.

  Note for anyone whose live run dies at "never wrote `.runtime_api_port` after 180s": that is the
  ONNX-runtime download into the fresh scratch directory, which `live-test.sh` documents right above
  the start block. Export `ORT_DYLIB_PATH` at an existing copy. It is not a hang in the feature.
- **P4 LANDED 2026-08-05** Keyfile encryption for secrets and connector tokens. XChaCha20-Poly1305
  envelope at `<data_dir>/secrets.json`, key at `<data_dir>/secrets/master.key` (0600 in a 0700
  directory, `POND_SECRET_KEY_FILE` to relocate). In-place migration, atomic tmp+rename, and a
  locked-not-emptied failure mode. See the LANDED block in 3.4 for the threat model and the
  key-loss story. Coverage of the four `api_key_*` fields still waits on P2.
- **P5 — LANDED 2026-08-05, gating five of eighteen senders, and saying so.** `NetworkMode`,
  `egress_verdict`, `check_egress`, `EgressDenied` and `EgressCall`/`begin`/`finish` in
  `shared/services/egress.rs`; `network_mode` on `Settings` with the full five-point plumbing;
  installed in `serve()` beside the early settings load and re-installed by `PUT /settings`, so a
  network restriction does not need a reboot to apply.

  **What shipped differs from what was designed in two ways worth stating.** The phase was written
  as "`network_mode` enforcement", which reads as though the setting existed; it did not — grep for
  `network_mode` returned nothing outside the vendored goose tree, so this phase created it. And
  `record_egress` had seven call sites in three files against **eleven** crates declaring `reqwest`,
  so the promised guard could not be the crate-level one. See 3.5.

  **`offline` is not yet a complete claim, and the code says so out loud.** Five sender files are
  gated (`pond-mcp-server/http.rs`, which covers all fifteen knowledge/news/finance/discovery tools;
  `pond-adapters-weather`; `fcm_push_relay` at both the token exchange and the send, because a token
  cached before the mode tightened would otherwise keep pushing; and **both** webhook executors,
  which are separate code and neither of which recorded egress at all before this — a scheduled
  webhook POSTs the whole task payload to a URL the user typed in and appeared in no activity feed).
  Six remain: HF/GitHub model downloads, the OAuth refresh loop, Spotify, the HF blob cache, the
  vision-encoder download and the MCP connectivity probe. They are enumerated in `UNGATED_SENDERS`
  in `crates/pond-core/tests/egress_guard.rs` under a cap that only moves down, and they belong to
  P6. *(True as written on 2026-08-05. P6a gated five of the six on 2026-08-06; only `routes.rs`
  remains, and P5's own claim that `record_egress` covers every sender was still incomplete in a
  way neither phase had noticed — see the `curl` subprocess note in 3.5.)* The remaining seven senders are loopback-only — ollama on 11434, llamafile on 8080 — and a
  guard that reported those as egress would be switched off inside a week.

  **The parse deliberately widens and the API deliberately narrows.** `NetworkMode::parse` falls
  back to `open` on an unrecognised value, unlike `PolicyMode::parse`, because `PolicyMode` has a
  middle tier (`audit`) that is wrong in neither direction and this setting does not: absorbing a
  typo into `allowlist` would take a home assistant off the internet with no diagnostic anyone could
  act on. `PUT /api/v1/settings` refuses an unrecognised `network_mode` with 422 — that is the
  narrowing half of the bargain, and it is what makes the permissive fallback safe. Everything else
  fails closed: an unparseable URL becomes `"unknown"`, which `classify_host` already calls
  `Sensitive`, which both restrictive modes refuse.

  **Mutation-tested, and the main guard failed its first mutation.** `production_source` was written
  as "everything before the first `#[cfg(test)]`", which is what the convention looks like. Adding a
  real `.send()` to `pond-api/src/middleware/mod.rs` **below** its trailing `mod tests` left the
  guard green — and eleven files in this tree already carry more than one `#[cfg(test)]`, so the
  truncation was silently discarding production code between them. It now removes `#[cfg(test)]`
  items rather than truncating, and the same mutation fails with `these files send HTTP and are in
  no list: ["crates/pond-api/src/middleware/mod.rs"]`. Vacuity: pointing `workspace_root` at a
  non-existent directory fails four tests on `no .../crates -- this guard scans the workspace and
  cannot run without it`, rather than reporting an empty set as clean. Other mutations: a
  `https://metrics.example.com` literal added to `pond-adapters-ollama` fails
  `loopback_exemptions_contain_no_third_party_url` naming the URL; disabling the `NETWORK_MODES`
  check makes the API test fail `200 != 422`; dropping the `upsert!` line makes
  `roundtrip_persists_every_field` fail with `network_mode: wrote "open-probe", read back "open"`;
  making the `Allowlist` arm permit everything fails the nine-cell matrix on `allowlist must refuse
  a Sensitive host`.

  **The live check found a real defect and then found a second one in itself.** `GET /api/v1/weather`
  rendered its error with `{e}`, which prints only the outermost `anyhow` context — so a refused
  call read `Failed to fetch weather: weather API request failed` and was indistinguishable from
  open-meteo being down. It renders `{e:#}` now, and the live run asserts the message names the
  setting, the mode and the host. The second defect was mine: the section opened with a warm-up
  `GET /weather` as a "does the provider exist" control, and `OpenMeteoWeatherAdapter` caches for 15
  minutes, so the refusal that followed was served out of memory and the gate was never consulted.
  The control is now the error message itself, which costs no round trip and cannot be cached.
  `section_network_mode` / `section_network_mode_after_restart` in `scripts/live_checks.py` carry
  both halves; the restart pass is where they have to live, because the weather provider is built
  once at startup from stored settings.

  Also fixed here: `MockSettingsRepository` round-trips `network_mode`. It overlays a hand-picked
  subset of fields, so without that the API test would have asserted a value that was never stored —
  the same reasoning already written beside `mic_enabled` in that file.

  Gates: fmt clean; pond-core 786 + 5 guard, pond-infra 213 + 3 + 3, pond-api 6 settings tests,
  pond-infra-scheduler, pond-mcp-server, pond-adapters-weather all green;
  `cargo check -p pond-server -p pond-adapters-goose` clean; `scripts/live-test.sh --ui` green,
  11 restart-pass checks, 0 failed.
- **P6 splits, because half of it was never blocked.** The bullet used to read only "draft gate for
  outbound connector actions (lands with PAI-8), and P3's third chokepoint", which routed the whole
  phase as PAI-8-blocked. The egress half was assigned to P6 by 3.5 and by P5's entry and appeared
  nowhere here, so a reader planning work would have skipped it. It is now **P6a** (landable, and
  landed) and **P6b** (genuinely blocked).

- **P6a — LANDED 2026-08-06, five of P5's six senders gated, plus one nobody had counted.**
  `pond-adapters-goose/src/extension_manager.rs` (the MCP connectivity probe, whose URI is typed by
  whoever adds the extension and is therefore the most attacker-influenced destination in the
  tree), `pond-adapters-goose/src/vision_encoder.rs`, `pond-server/src/main.rs` (the background
  OAuth refresh loop), `pond-server/src/model_download.rs` (all three sites, including the
  `face-onnx`-gated `buffalo_l.zip` one — a `cfg`'d sender is still a sender), and
  `pond-hf-cache/src/lib.rs` (both redirect loops). `UNGATED_SENDERS` is down from six entries to
  one and `MAX_UNGATED` from 6 to 1.

  **`pond-hf-cache` had no `pond-core` dependency.** That is the only new plumbing in the phase and
  it was recorded nowhere. The direction is inward and there is no cycle — `pond-core` pulls
  `pond-voice` plus serde/tokio/tracing and never reaches back — so the dep is correct rather than
  merely convenient.

  **Gated per redirect HOP, not once on the entry URL.** Both HF cache loops follow redirects by
  hand, reassigning `current` in a `for _ in 0..10`, precisely because the host changes mid-chain —
  that is why `should_send_auth_on_redirect` exists two lines away. A one-shot check on the entry
  URL would wave through exactly the case that matters: `huggingface.co` redirecting to a CDN.

  **Stated out loud rather than discovered: this is a behaviour change on `allowlist`.** Neither
  `huggingface.co` nor `github.com` is in `KNOWN_PUBLIC_SUFFIXES`, so both classify `Sensitive`, so
  `network_mode = "allowlist"` now refuses every model download. That polarity is correct and
  invariant 4 says public suffixes are added deliberately, not to soften a refusal — so the fix is
  an actionable message (`EgressDenied` already renders mode, host and what to set), not a wider
  allowlist. Anyone on `allowlist` who wants models must move to `open` for the download.

  **2026-09-24 -- the vision encoder no longer sends on its own.** Its raw `reqwest` fetch (one gate
  on the entry URL, redirects followed inside `reqwest`, no resume, no pin) was replaced by
  pond-hf-cache through `build_redirect_aware_client`, so each hop is gated like every other HF
  download, and `vision_encoder.rs` left `EGRESS_TRACKED` because nothing in it sends any more. The
  fetch now starts by itself -- in the serve process only, for the active chat model and for a model
  whose download has just finished -- so `network_mode` is its only consent. A refusal reaches the
  household as the setting and the host that blocked it, and a mode change during the ~941 MB
  transfer pauses it, because the progress callback asks the gate again on every chunk. The size is
  stated on the Download row and in the chat status line before and while it moves.

  **The largest hole was one the guard could not see.** `ensure_onnx_runtime()` downloads a ~100 MB
  ONNX Runtime tarball from github.com by shelling out to `curl`, so it never appeared in a scan
  that finds senders by looking for `reqwest`. It is gated with `check_egress` rather than
  `EgressCall::begin`, because the function is synchronous and `finish` -> `record_egress` reaches
  `tokio::spawn`, which panics with no runtime on the thread; refusals are still recorded, since
  that path is runtime-safe.

  **And gating it exposed that the gate was installed too late to matter.** `set_network_mode` had
  exactly ONE call site, inside `run_server`, and the mode is a process-global defaulting to `Open`
  — so `network_mode = "offline"` was a silent no-op for the whole of `pond chat` (which is also
  the terminal voice loop) and `pond setup` (which is almost entirely downloads). Worse, inside
  `run_server` itself `ensure_onnx_runtime()` ran ~40 lines BEFORE the install, so gating it there
  would have produced a mechanism that cannot fire — this programme already has two of those and
  did not need a third. All three entry points now install the mode from the settings row before
  their first fetch, and `ensure_onnx_runtime()` moved below the install on all three. Nothing
  between the old and new sites touches ONNX (`apply_face_recognition_defaults` sets env vars,
  `Database::init`, the HF-cache migration, the system-dep warning).

  > **CORRECTED 2026-08-06 (synthesis). There were FOUR downloading entry points, not three, and
  > the guard's own detector is what hid the fourth.** `run_models` (`pond models download`) calls
  > `model_download::download_file` twice — the model and its config sibling — and installed no
  > mode at all, so the gate P6a added inside `download_file` was inert there and a stored
  > `network_mode = "offline"` permitted a full model download. A privacy control failing OPEN,
  > which is the polarity invariant 3 forbids.
  >
  > The reason it was not merely missed but *locked out of the question*: the guard asked "which
  > functions call `ensure_onnx_runtime()`" and pinned the answer at three with an `assert_eq!`
  > vacuity control. A function that downloads by another route could not appear in the answer no
  > matter how the assertion was written. The detector now asks **which functions DOWNLOAD**
  > (`ensure_onnx_runtime();`, `model_download::download_file(`, `download_and_extract_ort(`), the
  > count is 4, and `run_models` installs the mode immediately after its `Database::init`. The two
  > download helpers are named in an explicit `DOWNLOAD_HELPERS` exemption rather than inferred,
  > so an entry point cannot slip into it by accident.
  >
  > The general lesson, which is this file's second instance of it: a vacuity control pins the
  > answer to whatever question the detector asks. If the question is narrower than the test's
  > name, the control makes the gap permanent instead of catching it.

  > **ALSO CORRECTED 2026-08-06 (synthesis): the ORT gate — this phase's headline discovery — had
  > no test of any kind.** The new guard asserted only the ORDER of `set_network_mode` against
  > `ensure_onnx_runtime()`; nothing asserted the ~100 MB github.com transfer was gated *at all*,
  > while the guard's own failure message talked about "the gate inside it". Deleting the
  > `check_egress` line from `download_and_extract_ort` left all six tests in `egress_guard.rs`
  > green — `egress_tracked_files_reach_the_tracker` is satisfied by the unrelated OAuth
  > `egress::begin(` elsewhere in `main.rs`. Since it is the only subprocess sender in the tree, no
  > `reqwest`-shaped detector will ever see it, so that was the whole of its coverage.
  >
  > `every_entry_point_installs_the_gate_before_it_downloads` now also locates the
  > `download_and_extract_ort` chunk and asserts `egress::check_egress(` appears at a byte offset
  > **before** `Command::new("curl")`. A refusal after the bytes are on the wire is not a refusal.

  **What this deliberately did NOT do.** `pond-api/src/routes.rs` is untouched: it holds nine egress
  sites (HF search, HF repo files, GitHub releases, the spawned download task, the TTS voice config,
  BOTH OAuth token exchanges — `authorization_code` as well as refresh, which the doc named nowhere
  — and all three Spotify sites including the post-401 retry) interleaved with seven loopback ones,
  and it was held by a concurrent group. It is P6b, and until it lands `MAX_UNGATED` is 1, not 0.
  `crates/pond-api/tests/egress_offline_routes.rs` was planned and NOT written, for the same reason.
  `run_agent_cmd` still never installs the mode, and `init_draft_authority` / `set_egress_sink` are
  still `run_server`-only — those are startup gaps of the same shape, recorded, not fixed here.
  `ensure_espeak_ng_data` and `fetch_buffalo_l_zip` are gated in source but have NO behavioural
  test: the first shells out to `brew` and has half a dozen environment-dependent early returns
  before it reaches the network, so a test of it would pass on any dev machine without touching the
  gate; the second only compiles under `--features face-onnx`.

  **Mutation-tested, and the main guard failed one of its own mutations.** `every_entry_point_
  installs_the_gate_before_it_downloads` is new, lives in `pond-core` because CI has no
  `cargo test -p pond-server`, and asserts ORDER rather than presence. Deleting the `run_chat`
  install left it GREEN — because the comment above the deleted call still contained the words
  "set_network_mode" and a substring search cannot tell prose from code. It now matches the call
  form `egress::set_network_mode(`, and the same mutation fails with "`run_chat(` calls
  ensure_onnx_runtime() ... but never calls set_network_mode". Restoring the pre-fix ORDER fails
  with "installs the egress gate at byte 3911 but calls ensure_onnx_runtime() at byte 2507".
  Deleting both HF-cache gates fails `egress_tracked_files_reach_the_tracker` naming that file.
  Adding a seventh `UNGATED_SENDERS` entry fails the cap AND the partition test. The mutation that
  matters most: gating only ONE of the HF cache's two hops keeps the file-level guard GREEN and
  fails `get_redirect_to_a_non_loopback_host_is_refused_at_the_hop` with a DNS error for
  `cdn.invalid` — the proof that per-file symbol presence is not coverage.

  > **CORRECTED 2026-08-06 (synthesis): the phase found this defect in its own ORDER guard and
  > never propagated the fix to the FILE-level guard, which is the one the whole classification
  > scheme rests on.** `TRACKER_SYMBOLS` held bare symbols (`record_egress`, `check_egress`,
  > `egress::begin`, …), so `egress_tracked_files_reach_the_tracker` was satisfied by COMMENT
  > PROSE. Every real gate could be deleted from a tracked file and all six tests stayed green,
  > provided one of the phase's own explanatory comments mentioned the token. FIVE of the ten
  > `EGRESS_TRACKED` files were vulnerable that way — and two of the five,
  > `pond-adapters-goose/src/vision_encoder.rs` and `pond-server/src/main.rs`, were made vulnerable
  > by comments **this phase added**. Demonstrated: removing both `egress::begin`/`finish` lines
  > from `vision_encoder.rs` while keeping the comment left "6 passed; 0 failed".
  >
  > Fixed two ways, because either alone is defeatable. `TRACKER_SYMBOLS` now holds CALL forms with
  > the opening paren, and a `strip_line_comments` pass (string-literal-aware, so a `"https://…"`
  > does not eat the rest of its line) runs before the search. `urls_in` deliberately keeps running
  > on the *uncommented* source: the `LOOPBACK_ONLY` entries' `non_target_urls` allowances name
  > install instructions that genuinely live in comments, and stripping them would report every one
  > as stale. The same mutation now fails with "these files are listed EGRESS_TRACKED but CALL none
  > of […] in code (comments do not count)".
  >
  > **The standing lesson, now twice-proven in this one file:** any source-text guard in this repo
  > that greps a bare symbol name has this defect. Match the call form, and strip comments.

  Gates: `cargo fmt --check` clean; pond-core 802 + 6 guard, pond-hf-cache 20 + 5, pond-api,
  pond-infra 214, pond-adapters-goose 105, pond-server lib/bins 81 all green; `cargo check -p
  pond-server -p pond-adapters-goose` and `cargo check -p pond-server --features face-onnx` clean;
  clippy clean on the touched crates. NOT run: `cargo test -p pond-server` in full, which does not
  compile at this commit for an unrelated pre-existing reason — `tests/live_feature_test.rs` calls
  `MemoryExtractionService::run` without the `&ProfileScope` argument added by `4a86244a` /
  `1cc7a8eb`. That file is untouched by this phase and CI never builds it.

- **P6b — PARTIALLY LANDED 2026-08-06. One of its three parts is done, one is no longer blocked as
  of 2026-08-11, and one genuinely cannot be designed yet. The ledger must say so or this reads as
  finished when two thirds of it is not.**

  P6b was always three things bundled behind one word. Split them:

  | part | state |
  |---|---|
  | the `routes.rs` egress sites left by P6a | **LANDED.** Not PAI-8-blocked; it only ever needed an uncontended tree. |
  | draft gate for outbound connector actions | **BLOCKED, and honestly so.** Not on a call site but on a *subject*: there is no connector in the tree, so there is no outbound connector action to gate. PAI-8's own phase list puts the first one at P4 (Google), and P3-P8 are unstarted. This is the one part of P6b that is waiting on work rather than on a decision. |
  | P3's third chokepoint — redaction before a body leaves the pond | **NO LONGER BLOCKED — corrected 2026-08-11.** The reason given here was that there is no call site to wire and PAI-8 creates the first one. PAI-8 P1 landed that day, and `IngestPipeline::new` takes a `Redactor` that is deliberately **not** an `Option` — a pipeline that cannot redact does not exist, so the chokepoint is in the type rather than in a branch somebody has to remember. What is still true is that nothing CONSTRUCTS a pipeline yet (`crates/pond-core/tests/context_pipeline_is_not_wired_yet.rs` asserts it), so the chokepoint is present and unreached, which is a different claim from blocked and a much shorter distance from done. The dependency was never two-way. |

  **`UNGATED_SENDERS` is empty and `MAX_UNGATED` is 0.** Every file in the workspace that
  `egress_guard.rs` sees sending HTTP now either reaches the tracker or is loopback-only with a
  checked reason. That is invariant 4 discharged to zero for the code this guard can see — not for
  the code it cannot, which is still `download_and_extract_ort`'s `curl` and anything Goose's own
  provider layer does inside the submodule.

  **Thirteen sites, not the nine the doc claimed, and the extra four are the interesting ones.**
  P6a's stamp counted nine egress sites and seven loopback; the real split at this HEAD is sixteen
  senders, thirteen now gated and three genuinely loopback. The four the count missed:

  - **`spawn_tracked_download`'s non-HF branch**, the chokepoint every non-HuggingFace model
    transfer actually passes through. Both download handlers also refuse synchronously *before*
    they spawn, because a refusal that only lands in a detached task shows up as a failed row in
    the progress tracker and is not an answer to "why is nothing downloading".
  - **`transcribe` and `calibrate_wake_word`'s whisper forwards**, and
  - **the `probe` helper**, which the plan for this phase explicitly said to leave alone on the
    grounds that its call sites are `127.0.0.1` literals. Two of the three are. The third is
    `state.whisper_url`, i.e. `settings.voice_whisper_url` — a free-text setting that merely
    *defaults* to loopback. A pond pointed at a remote ASR box was POSTing raw household audio to a
    third party through an ungated `.send()`, and `network_mode = "offline"` did nothing about it.
    Gating it costs nothing on a default install: `check_egress` classifies loopback `Internal`,
    which every mode permits. `check_egress` and not `begin` on those three deliberately — under
    the shipped default they fire on every utterance, and recording an `Internal` egress event per
    utterance is how a privacy feed becomes something nobody reads.

  **The Spotify `Option` was hiding a refusal inside "not connected".** `spotify_api_call` returned
  `Option<Response>`, so a network-mode refusal came back as a bare `{"connected": false}` — which
  sends the user to re-run an OAuth flow that cannot possibly succeed while the mode is what it is.
  It now returns `Result<_, SpotifyUnavailable>` with `NotConnected` and `Refused` as separate
  arms; `/music/now-playing` answers `error: "network_refused"` with the whole `EgressDenied` text
  and `/music/control` answers 502. An unactionable refusal is the failure PAI-2 invariant 1 exists
  to prevent, and folding it into an unrelated state is the worst version of it.

  **The three model-search handlers deliberately do NOT return 502.** Their contract is already
  HTTP 200 with an `error` string, `PondApiClient.request` throws `ApiError` on any non-2xx, and
  the callers of `searchGgufModels` are outside this phase's footprint. Returning 502 would have
  converted a privacy refusal into an unhandled rejection in the dashboard. The refusal text is
  carried in full either way; only the envelope differs, and it differs to match what the shipped
  UI already handles.

  **`run_agent_cmd` installs the mode, and needed a new guard because the existing one could not
  see it.** `pond agent chat` / `agent tools` / `agent extras` has read the settings row since it
  was written and ignored the one field on it that says whether the pond may talk to anybody. It is
  invisible to `every_entry_point_installs_the_gate_before_it_downloads` because that detector asks
  "which functions DOWNLOAD" and `run_agent_cmd` downloads nothing — it hands three arms to
  `build_goose_backend`, which wires the LLM provider, the weather adapter and the whole MCP tool
  surface. Downloading is one way to phone home; running a turn is the other, and it is the common
  one. `every_entry_point_installs_the_gate_before_it_builds_an_agent` asserts ORDER, not presence,
  over the three callers of `build_goose_backend` (`run_server`, `run_chat`, `run_agent_cmd`), with
  the count pinned.

  **The main guard is NOT the file-level one, and it could not be.**
  `egress_tracked_files_reach_the_tracker` looks for one tracker symbol per FILE; `routes.rs` has
  sixteen senders, so gating one of them turns that test green while fifteen still phone out. That
  guard says exactly this in its own doc comment. So `crates/pond-api/tests/egress_offline_routes.rs`
  carries two halves: a source-level test that pairs each `.send()` with a **distinct** preceding
  gate inside the same function, and behavioural tests that install `Offline` and drive six routes
  over real HTTP asserting a *refusal* rather than a failure. The source half is not redundant —
  four gated sites are unreachable from a router test at all (two inside a `tokio::spawn`ed
  download, both OAuth exchanges needing a live provider redirect or the internal-extension token).

  **Mutation-tested, and the main guard failed its first mutation — for a reason worth recording.**
  The planned regression was deleting the gate from the Spotify **post-401 retry only**, leaving
  the first attempt and the refresh gated: a `.send()` copy-pasted below an existing gated one,
  which is exactly how that retry was written in the first place. All eight tests stayed GREEN.
  The cause was in the guard, not the code: `GATE_CALLS` held both `egress::begin(` and
  `shared::services::egress::begin(`, and the second is a **superstring of the first**, so one real
  call produced two distinct byte offsets and every function counted twice as many gates as it had.
  A source-text guard whose patterns overlap each other double-counts, and double-counting is
  indistinguishable from correctness until something is missing. With the list de-overlapped the
  same mutation fails naming the site: *"`spotify_api_call(`: send #2 (byte 1428) has only 1
  gate(s) before it, and 1 earlier send(s) already consumed them."*

  Two more mutations. Deleting the `run_agent_cmd` install fails the new entry-point guard with
  *"`run_agent_cmd(action: AgentAction) -> Result<()> {` builds a Goose agent … but never calls
  set_network_mode"*. Deleting the `search_gguf_models` gate fails
  `hugging_face_model_search_is_refused_offline` — and it failed by **returning twenty real
  HuggingFace model records**, which is the behavioural half proving it is not vacuous: the request
  genuinely left the machine. All three restored byte-identical (`shasum -a 256` compared).

  **The lesson to carry, in the same family as P6a's:** a guard that greps bare symbols is
  defeatable by comments; a guard whose patterns overlap is defeatable by arithmetic. Both fail
  *open*, both look correct in review, and only mutation finds either.

  **What this deliberately did NOT do.** No UI. `network_mode` stays `HEADLESS_BY_DESIGN`, and the
  comment on it now records that its stated condition ("it gets a control when that list is empty")
  is **met** while the control is not built — reclassifying it `UI_WIRED` without the control would
  make the completeness test assert something untrue and silence the only guard on it. Building it
  needs `Settings.tsx` and `types.ts`, which belong to another phase. The three loopback senders
  (`tts_synthesise`, `sync_ollama_models`, `list_ollama_models`) are ungated on purpose and named
  individually in `UNGATED_LOOPBACK_SENDS`, each pinned to the loopback literal that has to stay in
  the function. No `/transcribe` behavioural test: it needs a multipart fixture, and the same
  configurable-host question is covered by the diagnostics probe.

  **What would falsify this.** A `network_mode = "offline"` pond that still completes an outbound
  request from any `routes.rs` handler. Or a `pond agent chat` run that reaches a third-party host
  with the setting stored as `offline`. Or — the quieter one — an `allowlist` pond whose HuggingFace
  model search silently returns an empty list with no `error` field, which would mean a gate that
  refuses without saying so.

  Gates: `cargo fmt --check` clean; `cargo test -p pond-core` and `-p pond-api` green (including
  `egress_guard` 7/7 and `egress_offline_routes` 8/8); `cargo clippy -p pond-api -p pond-core` adds
  no new warning (the three in `routes.rs` are pre-existing and untouched by this diff);
  `cargo check -p pond-server` clean. NOT run: `cargo test -p pond-server`, which still does not
  compile at this commit for the pre-existing `live_feature_test.rs` reason P6a recorded — a
  different phase in this round owns that. `scripts/live-test.sh` NOT run by this phase; the
  coordinator owns the live run and this change touches routes and startup wiring, so it needs one.
- **P7 — LANDED 2026-08-05.** `PUBLIC_ROUTES` entries carry an `Exposure`: `Always`,
  `UntilOnboarded`, or `UntilOnboardedThenHostOnly`. Nine entries are state-dependent — `PUT
  /settings`, `POST /profiles`, `PATCH /profiles/{id}`, `POST /onboard`, `/onboard/complete`,
  `/onboard/step/{name}`, both `/voice/calibrate` methods, and `POST /onboard/reset` in its own
  class. `GET /onboard/status` stays `Always`: a client must be able to ask whether it needs the
  wizard before it has anything to authenticate with.

  **Reset is the whole phase.** It stays reachable after onboarding, from the host only, and the
  gate reads onboarding state live on every state-dependent request so the closure re-opens the
  instant a reset lands. Section 3.7 records why both alternatives — a latch, or an unconditionally
  public reset — are defects rather than trade-offs.
  `reset_then_recover_is_not_a_one_way_door` drives the whole cycle (closed, reset from loopback,
  reopened, re-completed, closed again) on ONE router with no restart, which is what makes it a
  latch test rather than a state test.

  **Designed with a defaulted trait method; shipped without one.** The plan gave
  `OnboardingRepository::is_complete` a default body reading `get_current_step`. That is this repo's
  own named bug class: eight implementors, mostly test stubs, and a stub inheriting "not onboarded"
  makes every onboarding write route public wherever it is used. PAI-1 invariant 2 says access
  narrows on failure, and a default cannot know which way is narrow for the adapter it lands on. The
  method is **required**, so the compiler named all eight and each one decided out loud. Tedious,
  and correct.

  **A scope-widening default was found on the way.** `SqlxOnboardingRepository::get_current_step`
  ends `.ok()??`, so a database error is reported as "not started" — and "not started" is exactly
  the state in which every onboarding write hole is open. Keying auth on that would have re-opened
  all of them on a fully set-up pond whenever SQLite returned `BUSY`. `is_complete() -> Result<bool>`
  lets the error out and `pond_is_onboarded` treats a failed read as *onboarded*, i.e. closed. There
  are two tests: one in `pond-infra` that drops the table and asserts the two methods genuinely
  disagree about it, and one in `pond-api` that drives `PUT /settings` through the middleware
  against a repository that cannot read, and gets a 401.

  **The `Arc` blanket impl forwards it explicitly.** `AppState` holds
  `Arc<dyn OnboardingRepository>`, and method resolution picks the impl on `Arc` before the concrete
  adapter — so a defaulted method would have run the default body on the `Arc` while the SQLite
  override it was written for never executed, and nothing about the code would have looked wrong.
  Requiring the method makes deleting that arm a compile error rather than a silent behaviour
  change.

  **The guards changed shape, because the question changed.**
  `every_protected_route_requires_a_token` now asks `reachable_without_token_in_some_state` — a
  compile-time check has no pond to read, and a guard that picked one state would report the other
  state's answer as safety. `public_router_and_allowlist_agree` compares the union of all three
  classes against the router. A fourth guard, `the_public_route_classification_is_pinned`, asserts
  the exact list of state-dependent entries, so moving a route between classes is an edit somebody
  has to write down. None was weakened or deleted.

  **`POST /tts` is deliberately NOT closed, and the plan said to close it.** The wizard's voice
  preview is why it is public, which makes it look like an onboarding hole. Two shipped callers
  speak through it with no `Authorization` header long after setup — `playTtsSentence` in
  `pond-desktop/src/modes/voice/WebVoiceBackend.ts` and `fetch_tts_bytes` in
  `pond-desktop/src-tauri/src/commands/audio_cmd.rs` — so closing it would leave the assistant mute
  on a set-up pond. That those two are unauthenticated is a real finding; the fix is to give them
  the token, a client change, not an allowlist change. `POST /voice/calibrate` *is* closed because
  `calibrateWakeWord` in `PondApiClient.ts` attaches the bearer token — checked, not assumed, and
  that check is the only difference between the two decisions.

  **One existing test asserted precisely the behaviour this removes**:
  `put_settings_without_a_token_is_still_allowed` ran against `OnboardingStep::Completed`. Its
  assertion was right and its *fixture* was the hole. It is now two tests, one per state — the same
  lesson as `settings_is_blocked_before_onboarding` in P0, arriving from the fixture side. The
  onboarding fixture also swapped `MockSettingsRepository` for the real SQLite one: the mock stores
  a hand-written subset of `Settings` and drops `chat_model`, which `complete_onboarding` refuses to
  proceed without, so a wizard round trip against it could never finish — a fixture production
  cannot produce.

  **Mutation-tested.** Replacing the live read in `pond_is_onboarded` with a latch (a process-wide
  `EVER_ONBOARDED` flag set on the first completed read) fails
  `reset_then_recover_is_not_a_one_way_door` at step 3 with `assertion left == right failed: after a
  reset the wizard must be able to save again, left: 401, right: 200` — the one-way door, named.
  Restored; `cargo fmt --check` clean and 16/16 green afterwards.

  Gates: fmt clean; pond-core 783 + 5, pond-infra 214 + 3 + 3, pond-api 112 lib + all 17 integration
  targets green; clippy clean on the fast set; `cargo check -p pond-server -p pond-adapters-goose`
  clean; `scripts/live-test.sh --ui` green including the no-bypass auth pass, which now asserts the
  PAIR — `PUT /settings` returns 200 with no token *before* `POST /onboard/complete` and 401 after —
  plus the full reset-then-recover round trip over real HTTP.
- **P8a** LANDED 2026-08-06 — the would-deny telemetry the flip is waiting on.

  P8 was one bullet and it read as if only a release stood between `audit` and `enforce`. It did
  not: the telemetry that bullet presumed was emitted and never aggregated, never read, and gone in
  seven days. This is the half that makes the question answerable. It does not flip anything.

  **What landed.**

  1. *The verdict is a first-class field.* `SecurityPolicy::audit` takes `decision: &PolicyDecision`
     in place of `ok: bool`, and `DraftAuthority::audit` the same. Required, not defaulted, and with
     no parallel `audit_decision` beside the old one — the compiler named all four implementors
     (`AllowAllPolicy`, `SqliteSecurityPolicy`, `RepoDraftAuthority`, and the `draft.rs` test stub)
     and each decided out loud. `SqliteSecurityPolicy` now writes `verdict`, `mode` and `reason`
     beside the existing `ok`, still classified `Sensitive`. `reason` is **absent** rather than
     blank on a permit: an empty string reads as a field we failed to record.
  2. *The `:{verdict}` suffix came out of both action strings.* `routes.rs` writes
     `identify_session` and `draft.rs` writes `draft_approve` / `draft_reject`. Nothing parses a
     substring any more.
  3. *`AUDIT_ACTION` and the attribute keys moved to `pond-core`* (`security/ports/policy.rs`).
     They were a private const in `pond-infra` while nothing read the events. The moment something
     read them, one of two copies was going to drift and the reader would answer zero forever
     without failing. `VERDICTS` is exported for the same reason and has its own test asserting the
     producer and the buckets are the same set in both directions.
  4. *`POLICY_COUNTERS`* — process-lifetime `AtomicU64` tallies, recorded **at the two decision
     sites** rather than inside an `audit` implementation. Deliberate: this counts what the policy
     decided, and it must not stop counting because an audit sink is unwired. There is no `reset`.
  5. *`GET /api/v1/security/policy-report?window=hour|day|week`* — registered in `protected_routes`,
     absent from `PUBLIC_ROUTES`, so the compile-time route guards do the auth work. Two labelled
     blocks, never summed: `events` (durable, pruned at seven days, erasable by
     `DELETE /api/v1/activity`) and `process` (survives both, dies on restart). Zeros and 200 on an
     empty window, never 404.
  6. *Honest truncation.* `EventQuery` has no action filter and no group-by, so both happen in Rust
     over at most `POLICY_REPORT_MAX_EVENTS` (5000) rows, and the body carries `truncated`,
     `scanned` and `unclassified`. Recon caught this drift in the design: the plan said "an events
     query grouped by the verdict attribute" and the store cannot do either. A silently capped total
     would be the same lie the two-source split exists to avoid.

  **The guard, and the two mutations.** `crates/pond-api/tests/policy_report_test.rs` drives a real
  router with a live `SqliteEventLog` **and** a live `SqliteSecurityPolicy` — `security_policy: None`
  is what every other pond-api integration harness passes, and every assertion here would have
  passed vacuously against it. Four tests: the behavioural one (zeros before, `would_deny == 1` and
  `allow == 0` after, in both halves), the two-source separation (clearing the activity log zeros
  the events half and must not touch the process half), the window rejection, and 401 without a
  token.

  - *Mutation (a)* — flip `PolicyDecision::verdict()`'s `(true, true)` arm from `"would_deny"` to
    `"allow"`. FAILED with `the event half lost the would-deny: {"events":{"allow":1,…,"would_deny":0},
    "process":{"allow":1,…,"would_deny":0}}  left: 0  right: 1`. Exactly the confusion the field
    exists to prevent, named in the message.
  - *Mutation (b)* — put the verdict back on the action string and drop the `verdict` attribute.
    FAILED with `"would_deny":0,"unclassified":1,"scanned":1` and the process half still reading
    `"would_deny":1`. It did not crash, it counted the row it could not classify — which is what
    proves the endpoint reads the attribute and not a substring, and why `unclassified` is a
    separate bucket rather than folded into `allow`.
  - Both restored and verified byte-identical with `cmp`.

  **One thing the guard nearly got wrong, recorded because it is the shape this programme keeps
  hitting.** The first run of the file passed and the second failed with `process.would_deny` 2 vs 1:
  `POLICY_COUNTERS` is a process-global and `cargo test` runs the functions concurrently, so two
  tests taking a decision made each other's deltas wrong. A test that reports the scheduler is worse
  than no test. Both decision-taking tests now hold a `DECISION_LOCK` first.

  **Reachable in production.** `main.rs` binds `SqliteSecurityPolicy` over the real event log
  unconditionally (`security_policy` in `AppState`), `security_policy_mode` defaults to `audit`, and
  `PUT /api/v1/sessions/{id}/user` is the live route. A remote caller holding a handshake token gets
  `Principal::token`, which proves no membership, so **every remote "this is Liz" is a would-deny
  today** and lands in both halves of the report. The draft path counts on every approve/reject.

  **What `scripts/live_checks.py :: section_policy_telemetry` can and cannot reach.** live-test.sh
  runs the functional pass with `POND_DEV_ALLOW_LOOPBACK`, so the middleware attaches
  `Principal::loopback()` — and `is_identity_assertion_proven` deliberately permits loopback. So the
  decision that section takes is an **allow**, and a would-deny is not producible from there. It
  asserts the allow moved by exactly one in *both* halves, that `events.available` is true (the
  event log is really bound), that `unclassified` is zero (nothing wrote an audit row the report
  could not read), and that the count was not capped. Claiming it demonstrates a would-deny would be
  the unreachable-fixture mistake in reverse. The would-deny is covered by the integration test,
  where the principal is a `Token`.

  **Deliberately NOT done:** any `pond-desktop` UI, any new `Settings` field
  (`security_policy_mode` stays HEADLESS_BY_DESIGN, unchanged — `curl` is the operator path), any
  change to the retention or erasability of the audit trail, and the P8b flip. Also not done:
  auditing anything beyond the two call sites that already audited. The report counts what is
  recorded; it does not widen what is recorded, and it must not be read as coverage of the whole
  cross-boundary surface.

  **A drift corrected in passing.** `sqlite_security_policy.rs`'s module docs claimed nothing called
  `SecurityPolicy::audit` in production. That stopped being true when P1 landed. Fixed in the same
  change, with the two real call sites named.

  **What would falsify this.** If `GET /api/v1/security/policy-report` ever reports a would-deny
  total that `DELETE /api/v1/activity` can take to zero, the two-source split has been collapsed and
  the endpoint is lying by omission — `clearing_the_activity_log_cannot_zero_the_process_counters`
  is the test that says so. And if `events.unclassified` is ever non-zero on a live run, something
  is writing audit rows without a verdict and the report is undercounting silently.

  Gates: `cargo fmt --check` clean; clippy clean (0 errors) on pond-core / pond-infra / pond-api /
  pond-mcp-server `--all-targets`; pond-core 808 + 6 + 2, pond-infra 217 + 3 + 4, pond-mcp-server 185,
  pond-api 115 lib + all 21 integration targets including the new 4, all green;
  `cargo check -p pond-server -p pond-adapters-goose` clean. **No live run** — the coordinator owns
  it, and this phase adds a route and a handler, so it needs one.
- **P8b** BLOCKED on P8a — flip the default to `security_policy_mode = "enforce"`, only after a
  release in `audit` whose telemetry shows what would have been denied.

  **The standing precondition still holds and its REASON changed on 2026-08-11.** It read "no schema
  links a paired device to a member", and that is no longer true: migration 0043 put `profile_id` on
  `devices`, and PAI-1 P9's identity half wired the rung end to end, so a turn now knows which member
  its paired device belongs to. `is_identity_assertion_proven` still refuses every remote explicit
  identification anyway, for a different and much smaller reason: it reads
  `Principal.proven_profile_id`, and the auth middleware still sets that to `None` on every path.
  The device is surfaced on the `Principal`; the *member* is resolved downstream in
  `resolve_turn_scope`, which is the turn's scope and not the policy layer. Two consumers, one rung,
  and only one of them reads it.

  So flipping before that is threaded would still break "this is Liz" from a phone on every pond —
  the outcome is unchanged — but the remaining work is one attribution lookup on the way into the
  `Principal`, not a schema. Worth re-checking before anyone plans around it: `grep -n
  "proven_profile_id:" crates --include='*.rs' | grep -v None` returns only the field declaration.
- **DEFERRED** SQLCipher for the full database.

---

## 5. Invariants

1. A deny is logged with its principal, scope, action and outcome — an unexplained refusal is worse
   than no refusal.
2. Secret values never appear in a REST response, a log line, an event attribute, or a prompt.
3. The redactor never runs on the model's prompt.
4. Egress classification stays fail-`Sensitive`. New allowlist entries are added deliberately, with
   the exact-or-dotted matcher preserved.
5. `enforce` mode never locks a user out of `/handshake/*` or `/health` — recovery must stay
   reachable.

---

## 6. Deliberate deferrals

- **Full-database encryption.** See 3.4.
- **Model-based PII detection.** Costs inference on the critical path; the rule-based redactor
  handles the shapes that actually leak.
- **Per-extension capability sandboxing** (an extension declaring which scopes it needs). Attractive,
  and much easier once the policy matrix exists — revisit after P8.

---

## 7. Verification

- **Unit** — the two real policy rules, one test per meaningful case:
  `is_identity_assertion_proven` (a token principal may not assert a member it has not proved;
  loopback may assert anyone; internal may assert nobody) and `is_draft_decision_permitted` (the
  four rungs: no actor, `Guest`, an owned draft, an unowned draft), plus the recovery routes that
  must never be denied.

  *Corrected 2026-08-06.* This bullet used to read "the policy matrix, one test per scope ×
  principal-kind cell". **There is no matrix and there deliberately never was one** — P1 argued it
  out in `is_identity_assertion_proven`'s doc comment and built the two rules instead: eight scopes
  crossed with three `PrincipalKind`s gives twenty-four cells that all have to be `allow`, because
  each kind legitimately needs each scope for something in the code today and denying `Internal`
  anything breaks background work silently. Twenty-four allows is not a security control, it is a
  table that looks like one. Section 7 was never updated to match, so this listed as *planned
  verification* a structure the implementation had already rejected on the record.
- **Guard tests** — no secret-shaped settings key is serialized; every HTTP-sending source file is
  classified as tracked, loopback-only, or knowingly ungated. Both must fail the build, not warn.
  Corrected 2026-08-05: the crate-level form of the second guard ("every `reqwest`-using crate
  references `record_egress`") is not implementable — see 3.5.
- **Redactor** — a corpus with true positives and deliberately hard negatives; assert idempotence
  (redacting twice equals redacting once).
- **Integration** — `network_mode = "offline"`, run a weather query, assert a clean refusal with an
  actionable message rather than a timeout.
- **Manual** — `GET /settings` on a pond with every API key set; assert not one key value appears in
  the response body.
