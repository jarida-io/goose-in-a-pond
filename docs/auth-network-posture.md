# Authentication and network exposure

How `pond-server` authenticates clients and what it exposes on the network.
Covers issues #4, #8, #93, #94.

## Bind & exposure

`pond-server` binds `0.0.0.0:<API_SERVER>` (default 4000), so it is reachable
from the **local network**, not just loopback. Everything below assumes that
LAN reachability. W3 restricts unauthenticated diagnostics and device-scoped
notification delivery; it does not encrypt this listener. Do not expose a
plaintext listener through a tailnet or the internet. The separate pinned-HTTPS
work in [PR #375](https://github.com/jarida-io/goose-in-a-pond/pull/375) supplies
loopback-only HTTP and an API-only HTTPS listener. These authorization checks
apply independently of transport.

## Authentication

- All `/api/v1/*` routes require `Authorization: Bearer <session_token>` and are
  rejected with **401** otherwise, **except** the public allowlist: `/health`,
  `/handshake`, `/handshake/{init,verify,refresh,pairing-code}`,
  onboarding routes, and a few local dev/test pages
  (`crates/pond-api/src/middleware/mod.rs::route_exposure`). The allowlist is
  state-scoped rather than flat: each entry in `PUBLIC_ROUTES` carries an
  `Exposure` of `Always`, `HostOnly`, `Authenticated`, `UntilOnboarded`, or
  `UntilOnboardedThenHostOnly`. `Authenticated` keeps revocation available before
  onboarding finishes without making it public. `HostOnly` requires a token from
  network peers; only the actual loopback connection gets the compatibility
  exemption. Missing connection metadata is treated as remote, and forwarding
  headers do not change this classification.
- Tokens are validated against the DB-backed `SqliteHandshakeAdapter`
  (`validate_token`): only unrevoked, unexpired session tokens pass.
- Session tokens expire after 24h; refresh tokens after 30d. Clients rotate via
  `POST /api/v1/handshake/refresh` (rotation revokes the old session).

### Revocation and device-scoped delivery

`POST /api/v1/handshake/revoke` requires the current session bearer token. It
revokes that session row, including the associated refresh credential. The
request body does not choose a token: older GOTG clients may continue sending
`{token: ...}`, but only the bearer is used. Missing, expired and revoked bearer
credentials return 401. The endpoint works before and after onboarding.

`GET /api/v1/notifications/stream?device_id=...` and
`POST/DELETE /api/v1/devices/{id}/push-token` require the target device to match
`Principal.device_id`, obtained by the middleware from `caller_for_token`.
A mismatch (including absent token attribution) returns 403 `device_mismatch`
before queue access, device lookup or push-token mutation. A caller cannot use
these routes to discover whether someone else's device exists. Claims are not
copied into the principal, and smart-home device targets are not confused with
the identity of a companion phone. No IP address binding is added; an issued
session can roam and refresh remotely.

This is bearer authorization, not device-key proof of possession. A stolen
session can still act as its recorded device until expiry or revocation. Refresh
is possession-based; an already open SSE connection is not reauthenticated per
event. Closing active streams on credential revocation and addressing concurrent
refresh/revoke races require a separate session-lifecycle change. A client with
an expired session must refresh before server-side logout; local credential
removal alone is not a server revocation guarantee.

### Diagnostics and bootstrap

Transcription (`POST /transcribe`) and agent status (`GET /dev/goose`) require
authentication and completed onboarding. `/tts`, `/test`, `/test/speak`, and
non-API dashboard/development pages allow anonymous **loopback** callers only;
remote callers need a valid token. This preserves local desktop/CLI speech.
Health, onboarding status and `/system/info` remain public for connection and
pairing bootstrap; discovery information must never establish trust in a new TLS
key. Existing onboarding and OAuth-specific guards remain in place.

Denials emit a structured `device_mismatch` warning with the operation name;
accepted device checks emit debug events, and successful revocation emits an
info event. Tokens, notification contents and claimed identifiers are excluded
from those events. The API error code is stable for client localization.

## Pairing (how a client gets a token)

Two-phase, HMAC-based — the 6-digit pairing code is **never sent over the wire**:

1. Operator reads the pairing code printed on server startup (or `GET
   /api/v1/handshake/pairing-code`, loopback-only).
2. Client `POST /handshake/init {client_id,…}` → `{challenge_id, challenge}`.
3. Client computes `mac = HMAC-SHA256(pairing_code, challenge ‖ client_id)` and
   `POST /handshake/verify {challenge_id, mac}` → `{session_token, refresh_token,
   expires_at}`.

Codes are single-use and expire in 10 min; a challenge expires in 60 s. Only
sha256 hashes of codes/tokens are persisted.

**There is no failed-attempt lockout, and its absence is deliberate.** A bad MAC
burns the *challenge*, not the code: `verify_handshake` consumes the challenge
before it checks the MAC (`sqlite_handshake.rs:355-371`) and returns
`invalid_mac` with the pairing code untouched (`:414-419`), which is consumed
only on success (`:435-439`). The `pairing_codes` table has no attempt counter,
and `bad_mac_attempts_do_not_lock_out_pairing_code` (`:955`) asserts a
legitimate pairing still succeeds after ten bad guesses. A lockout would hand
any guest on the wifi a denial of service against the operator's own pairing.

### Token contract

`HandshakeResponse` uses **`session_token`**, **`refresh_token`**, and
**`expires_at`** (absolute RFC3339). We standardised on absolute expiry rather
than relative `expires_in` to avoid clock-skew / round-trip drift; server,
GOTG, and desktop clients all use this shape.

## Loopback

The blanket loopback auth bypass was **removed** (#94). By default, even
same-host clients (including the desktop app) must present a valid token —
the desktop auto-pairs via the loopback `pairing-code` endpoint.

For local development you can opt back into the bypass with:

```
POND_DEV_ALLOW_LOOPBACK=1
```

This is **off by default** and intended only for dev machines.

## CORS

Scoped to first-party origins (`app://giap`, `http://localhost:1420`,
`http://127.0.0.1:1420`); **not** `Any`. Add extra browser origins (e.g. a LAN
dashboard) with a comma-separated:

```
POND_CORS_ALLOWED_ORIGINS=https://dashboard.lan,https://…
```

`app://giap` is the packaged desktop app. Its renderer is deliberately served
from a privileged custom scheme rather than from `file://`, and the reason is
this list: a `file://` page sends `Origin: null`, which cannot be
allow-listed in any meaningful way and would have forced the allowlist open to
`Any` — undoing the scoping this section exists to describe. The two `:1420`
entries are the Vite dev server, which the desktop shell loads instead of the
packaged bundle during development.

Native mobile clients (GOTG) don't send a browser `Origin` header, so CORS does
not apply to them.

## Rate limiting

Three buckets, not one. The first two are mutually exclusive — handshake traffic
is selected into its own bucket and never touches the general allowance
(`pond-api/src/lib.rs:653-657`).

| Bucket | Limit | Covers | Loopback |
|---|---|---|---|
| General | 600 / 60 s per IP | everything except `/api/v1/handshake*` (`lib.rs:536-539`) | exempt |
| Pairing | 30 / 60 s per IP | every `/api/v1/handshake*` route (`lib.rs:544-547`) | exempt |
| Verify | 10 / 60 s per IP | `/handshake/verify` only (`routes.rs:590-595`) | **not exempt** |

Pairing was split out in `a2c86a93` because a chatty client spending the shared
allowance would lock a device out of `/handshake` — the recovery path. The verify
limiter sits inside the handler rather than the middleware, so it applies to
loopback too: the endpoint is security-sensitive regardless of origin.

The 429 does not have one shape. General and pairing go through
`AuthError::RateLimitExceeded` and set a `Retry-After` **header**
(`middleware/mod.rs:49-53`); verify is built in the handler and puts the same
figure in a `retry_after_secs` **JSON body field** with no header
(`routes.rs:681-688`). A pairing client has to read both, and since verify is
the tighter bucket it is the 429 such a client will actually see. Making the
two uniform is a behaviour change and belongs in its own PR.

Brute force is bounded by that verify limiter plus one-challenge-per-attempt, not
by a lockout. The bound is **per source IP**, like the table above: the limiter
keys on the TCP peer address (`routes.rs:676-678`), so every distinct address
gets its own bucket. At 10 attempts / 60 s against a code that lives 10 minutes,
one address is worth roughly 100 guesses out of 1,000,000 — about 0.01% of the
key space — and each guess burns its own challenge, so attempts cannot be
pipelined. An attacker holding N addresses gets 100N. That matters for the threat
model named above: a wifi guest cannot spoof a source address through a TCP
handshake, but can hold several without effort — a second DHCP lease, a static
address in the subnet, or IPv6 privacy addresses, which rotate on their own. On a
typical /24 the worst case is nearer 2.5% of the key space than 0.01%.
