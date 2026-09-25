# Authentication and network security posture

How `pond-server` authenticates clients and what it exposes on the network.
Covers issues #4, #8, #93, #94.

## Trust boundary and transport

There are three zones: loopback, directly attached LAN, and tailnet. The local
HTTP listener binds `127.0.0.1` (normally 4000); its dashboard, root development
pages, desktop integration, and OAuth callbacks are local. The companion API
listener uses HTTPS (normally 4443) on IPv4 network interfaces. Both share the
same authentication and rate-limit middleware. Each port has ten consecutive
fallback choices; `.runtime_api_port` and `.runtime_https_port` record the actual
ports. Implementation: `pond-server/src/main.rs::run_server`, `ports.rs`, and
`pond-api/src/lib.rs::build_transport_router`.

TLS terminates on the Pond with a persisted ECDSA P-256 key. The native companion pins the
public key delivered through the local v2 pairing QR and still checks certificate
validity and hostname SANs. Annual certificate renewal retains the key; replacing
the key requires local re-pairing. WireGuard protects remote transport underneath
HTTPS. Headscale node certificate issuance is not required. See
[remote access](remote-access.md) for deployment and certificate recovery.

Do not publish the listener on the public internet. An untrusted tailnet node
can reach public API routes; encrypting transport does not authenticate its user.
The broader W3 authorization work remains separate: bearer-token/device binding,
the revoke endpoint contract, and narrowing the public allowlist are not changed
by this milestone. Non-API pages are absent from the companion router.

## Authentication

- All `/api/v1/*` routes require `Authorization: Bearer <session_token>` and are
  rejected with **401** otherwise, **except** the public allowlist: `/health`,
  `/handshake`, `/handshake/{init,verify,refresh,revoke,pairing-code}`,
  onboarding routes, and a few local dev/test pages
  (`crates/pond-api/src/middleware/mod.rs::route_exposure`). The allowlist is
  state-scoped rather than flat: each entry in `PUBLIC_ROUTES` carries an
  `Exposure` of `Always`, `UntilOnboarded`, or `UntilOnboardedThenHostOnly`, so a
  route open during onboarding can close afterwards.
- Tokens are validated against the DB-backed `SqliteHandshakeAdapter`
  (`validate_token`): only unrevoked, unexpired session tokens pass.
- Session tokens expire after 24h; refresh tokens after 30d. Clients rotate via
  `POST /api/v1/handshake/refresh` (rotation revokes the old session).

## Pairing (how a client gets a token)

The Android and iOS clients use two-phase HMAC pairing; they do not transmit the six-digit code directly:

1. Operator reads the pairing code printed on server startup (or `GET
   /api/v1/handshake/pairing-code`, loopback-only).
2. Client `POST /handshake/init {client_id,…}` → `{challenge_id, challenge}`.
3. Client computes `mac = HMAC-SHA256(pairing_code, challenge ‖ client_id)` and
   `POST /handshake/verify {challenge_id, mac}` → `{session_token, refresh_token,
   expires_at}`.

Legacy handshake, initialization, and verification require loopback or a peer
within an active directly attached LAN interface's netmask. Tunnel interfaces,
point-to-point links, and tailnet allocations are excluded. Missing connection
metadata or failed interface classification is denied. Forwarding headers are
ignored. The rejection is HTTP 403 with `pairing_requires_lan`, including remote
completion of a challenge initialized locally. Refresh is still remotely usable.
See `pond-api/src/network.rs` and the handshake handlers in `routes.rs`.

Codes are single-use and expire in 10 min; a challenge expires in 60 s. The adapter hashes codes and tokens for validation; consult `sqlite_handshake.rs` for the storage contract.

**There is no failed-attempt lockout, and its absence is deliberate.** A bad MAC
burns the *challenge*, not the code: `verify_handshake` consumes the challenge
before it checks the MAC (`sqlite_handshake.rs:355-371`) and returns
`invalid_mac` with the pairing code untouched (`:414-419`), which is consumed
only on success (`:435-439`). The `pairing_codes` table has no attempt counter,
and `bad_mac_attempts_do_not_lock_out_pairing_code` (`:955`) asserts a
legitimate pairing still succeeds after ten bad guesses. Pairing is now limited to direct LAN peers. A lockout would still hand
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

## Residual risks and deferred authorization work

A stolen bearer token is usable until expiry or revocation; session validation
is not proof of possession of a device key. Refresh remains possession-based
with a 30-day window. A compromised tailnet member can reach the API's existing
public allowlist, including some information and development endpoints. Rate
limiting and transport encryption do not remove those authorization risks.
W3 must review them and update allowlist drift tests in the same change.

Native Android and iOS implement public-key pinning in this milestone. Browser
pinning is not implemented. iOS simulator transport verification passes; physical
iPhone roaming remains deferred until a device is available. Source-masking proxies/subnet routers cannot be
used to establish that a pairing peer is local. A compromised Pond OS or phone
can expose private keys or credentials; pinning cannot protect either endpoint
from its own compromise.
