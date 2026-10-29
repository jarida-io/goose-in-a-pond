# Authentication and network security posture

How `pond-server` authenticates clients and what it exposes on the network. Related:
[remote access](remote-access.md) (the tailnet, certificates, enrollment),
[pond networking](architecture/pond-networking.md) (claiming a pond with no screen),
[the setup channel](architecture/pond-setup-channel.md) (BLE),
[connections](architecture/pond-connections.md) (Wi-Fi settings) and the
[API reference](api.md) ("Pairing and tokens") for wire shapes.

## Listeners

Composed in `crates/pond-server/src/listeners.rs::compose`, bound in `main.rs::run_server`.

| Listener | Binds | Router | Who it is for |
|---|---|---|---|
| Dashboard | `127.0.0.1`, default 4000 (`serve --port`), nine fallbacks | `pond_api::build_router` plus remote-access management, behind `host_guard::loopback_only` | desktop app, browser on the host, CLI, OAuth callbacks |
| Companion | `0.0.0.0` (IPv4, every interface), default 4443 (`serve --https-port`), nine fallbacks, TLS | `pond_api::build_companion_router` plus phone enrollment; no static pages | phones on the LAN |
| Tailnet | private Unix socket (`pond-net-*/api.sock`) the `pondnet` helper forwards to, naming each request's tailnet machine | the companion router, taken before presence is layered; a token is accepted only from its device's enrolled machine | phones away from home |
| Plaintext dev | `0.0.0.0:4080`, debug builds with `POND_DEV_INSECURE_LAN=1` only | the companion router without enrollment or presence | Expo Go ([below](#the-plaintext-development-listener)) |

Ports come from `crates/pond-server/src/ports.rs` (`bind_with_fallback` tries the base and the
next nine). The bound ports are written to `<data_dir>/.runtime_api_port` and
`.runtime_https_port`. Both API routers share the same auth middleware, rate limits and CORS
layer (`pond_api::build_transport_router`). `crates/pond-server/tests/listener_wiring.rs` asserts
which extensions each listener carries.

TLS terminates on the Pond with a persisted ECDSA P-256 key (`tls/identity.json`,
`crates/pond-server/src/tls_identity.rs`). Certificates are self-signed, valid a year, and
renewed with the same key within 30 days of expiry or when the address SANs change, so the pin
phones hold (SHA-256 of the SubjectPublicKeyInfo, `sha256/<base64>`) never changes; replacing the
key means re-pairing every phone. The helper serves a second certificate from the same key on the
tailnet (`tls/tailnet-identity.json`) that names only tailnet addresses and `goose-in-a-pond`.
Details are in [remote access](remote-access.md#the-ponds-certificates).

Do not publish any listener on the public internet. An untrusted tailnet node can reach the public
routes; encrypting the transport does not authenticate its user.

## The loopback dashboard listener

Binding `127.0.0.1` keeps other machines out, not other software on this one. Three checks close
the rest (`crates/pond-api/src/host_guard.rs`, layered outermost on the dashboard router):

- **Host.** A `Host` (or HTTP/2 authority) other than `127.0.0.1`, `localhost` or `[::1]` gets
  `421 loopback_host_required`. This defeats DNS rebinding. The port is not checked, so
  `ssh -L 9000:localhost:4000` works.
- **Origin.** A present `Origin` must be a CORS origin ([CORS](#cors)) or `http://` with exactly
  the host and port the request was sent to (host compared without case, port 80 when absent),
  else `403 origin_not_allowed`. This defeats cross-site `POST`s that need no preflight, and a page
  served on another loopback port, which may be another account's.
- **Host credential.** Some routes also need `X-Pond-Host-Credential`: 32 random bytes,
  base64url (43 characters), written to `<data_dir>/.runtime_host_credential` (`0600`, atomically
  replaced, a planted symlink replaced rather than followed) at every start, before
  `.runtime_api_port` (`crates/pond-server/src/host_credential.rs`). Another OS account cannot
  read the file. A missing or wrong credential is `403 host_credential_required`; a listener wired
  without one (the companion listener) answers `503 host_credential_unavailable`.

| Route | Rule |
|---|---|
| `GET`/`POST /handshake/pairing-code` | loopback peer and the credential |
| `/remote-access*` (status, enable, disable, identity, register, device, recovery requests) | dashboard listener only, the credential |
| `GET`/`POST`/`DELETE /uber/accounts*` | loopback peer and the credential |
| `POST /onboard/reset` once onboarded | a valid bearer token, or the credential |
| `POST /tts` | a valid bearer token, or the credential |
| `POST /test/speak` | the credential, token or not |
| `PUT /devices/{id}` on a paired client's row (`gotg`, `desktop`, `cli`) | that device's own token, or the credential (`device_edit::may_edit`) |
| `/connections` writes | the credential, or the claimant's or a household member's device ([connections](architecture/pond-connections.md)) |

Where a route admits "a token or the credential" the handler checks the bearer itself
(`routes.rs::bearer_or_host_credential`), because the middleware skips the token for a loopback
peer on those routes.

**Who holds the credential.** The desktop shell reads the file over IPC (`host_credential` in
`pond-desktop/src/shell/contract.ts`) for every request. A browser gets it from the link
`pond-server dashboard` prints, `http://127.0.0.1:<port>/#host=<credential>` (not `localhost`,
which a browser may try as `[::1]` first). The fragment is never sent or logged; the page moves it
into session storage (`giap-host-credential`), takes it out of the address bar, and picks up a new
link opened in a running tab through `hashchange`. The link stops working at the next restart.
`pond-server pairing` reads the file itself. The Jetson kiosk (`scripts/jetson/kiosk.sh start`)
waits up to 30 s for `.runtime_api_port`, the credential and an answer from `/api/v1/health`, then
opens a `0600` page in `$XDG_RUNTIME_DIR` (or `snap.firefox` inside it) that forwards to the
sign-in link, so the credential never appears on a command line. `GIAP_KIOSK_URL` opens exactly
the URL given, with no sign-in; `GIAP_KIOSK_PORT` overrides the port. The kiosk's user unit is
`PartOf=goose-in-a-pond.service`, so a server restart, which rotates the credential, restarts it.

Software running as the Pond's own OS account can read the credential; no application-level
control protects against that, and none is claimed.

**The desktop shell's IPC.** Every `giap:` command is refused without running its handler unless
it comes from the main frame on `app://giap` (or the origin `GIAP_DEV_SERVER` names in
development) (`pond-desktop/electron/main/ipc.ts::senderRefusal`).
`electron/main/navigation.ts` cancels main-frame navigations and redirects to any other origin,
hands http(s) destinations to the system browser, and drops other schemes.

## Authentication

Every `/api/v1/*` route needs `Authorization: Bearer <session_token>` unless its entry in
`PUBLIC_ROUTES` (`crates/pond-api/src/middleware/mod.rs`) admits the caller. Each entry carries an
`Exposure`:

| Exposure | Tokenless caller admitted | Routes |
|---|---|---|
| `Always` | anyone | `GET /health`, `POST /handshake` (legacy), `/handshake/{pake/init,pake/verify,init,verify,refresh}`, `GET`/`POST /handshake/pairing-code`, `POST /handshake/spoken-code` (handlers then gate by peer), `GET /onboard/status`, `GET /time/zones`, `GET /system/info`, `GET /oauth/callback` (PKCE nonce), `POST /oauth/refresh` (internal extension token) |
| `HostOnly` | a loopback peer | `POST /tts`, `GET /test`, `POST /test/speak`, `POST /player/command`, `GET /player/status`, `POST /extension/egress`, and every path outside `/api/` (dashboard and dev pages) |
| `UntilOnboarded` | a loopback peer while the Pond is neither claimed nor onboarded | `POST /onboard`, `/onboard/complete`, `/onboard/step/{name}`, `/location/detect`, `PUT /settings`, `GET /warmup`, `POST /voice/tts/apply`, `POST`/`DELETE /voice/calibrate`, `POST /profiles`, `PATCH /profiles/{id}` |
| `UntilOnboardedThenHostOnly` | a loopback peer while unclaimed, or once onboarded | `POST /onboard/reset` |
| `Authenticated` | nobody; listed only to skip the onboarding guard | `POST /handshake/revoke`, `GET /settings`, `POST /voice/sample`, `GET /network/status`, `/connections*`, `GET /setup/status`, the five model routes completion needs, `GET /devices/self`, `POST`/`DELETE /devices/{id}/push-token`, `GET /notifications/stream` |

Everything not listed needs a token and completed onboarding (`403 onboarding_required` from
`middleware/onboarding_guard.rs`), including `POST /transcribe` and `GET /dev/goose`.

- **The peer.** "Loopback" is the TCP peer from `ConnectInfo`. Missing connection metadata counts
  as remote, forwarding headers are ignored, and a request on the plaintext development listener
  is never loopback. Two drift tests guard the table: `public_router_and_allowlist_agree` and
  `every_protected_route_requires_a_token`.
- **State is read live.** Onboarding and the claim are read on each request to a state-dependent
  route; a failed read counts as onboarded or claimed, which narrows access, and a Pond with no
  setup record reads as unclaimed.
- **Tokens.** Validated against `SqliteHandshakeAdapter::validate_token`: unrevoked and unexpired.
  Only SHA-256 hashes of codes and tokens are stored. Session tokens live 24 h, refresh tokens
  30 days; `POST /handshake/refresh` rotates both and revokes the old pair. `HandshakeResponse`
  carries `session_token`, `refresh_token` and an absolute RFC 3339 `expires_at`.
- **The principal.** The device id comes only from `caller_for_token`, never from the request.

### Who may drive the wizard

| Tokenless caller | Unclaimed, not onboarded | Claimed, not onboarded | Onboarded |
|---|---|---|---|
| Wizard routes (`UntilOnboarded`) | loopback only | nobody | nobody |
| `POST /onboard/reset` | loopback only | nobody | loopback, plus a token or the credential |

A valid token is admitted before onboarding in every state. A Pond is claimed once any device has
paired, so the desktop app drives the wizard before it pairs, and a phone claims first and runs
owner setup as itself. A browser tab on the host pairs with the credential from the
`pond-server dashboard` link. A claimed Pond that was reset and lost every token is not stuck:
pairing answers in every state, and `pond-server setup-code --rearm` makes it unclaimed. When
`POST /onboard/complete` succeeds, the device that claimed the Pond (`claimed_by` in the setup
record), if still unattributed, is attributed to `primary_profile_id`.

### Revocation and device scope

- `POST /handshake/revoke` takes the bearer only (an old body `token` is ignored), queues the
  device's tailnet revocation durably (`503 revocation_unavailable` if it cannot), revokes every
  session and refresh row of the token's device, then removes the device from the registry.
- `DELETE /devices/{id}` queues the tailnet revocation (`503 revocation_unavailable`), revokes every
  session through `Handshake::revoke_device`, and only then drops the row; a failed revocation
  refuses the delete. Matter devices never enroll and are not queued. `session_tokens.device_id`
  has no foreign key, so nothing cascades by itself. `revoke_device` deliberately has no default
  on the port: `Ok(0)` would read as "revoked nothing" and the row would go while access stayed.
- `GET /notifications/stream?device_id=` and `POST`/`DELETE /devices/{id}/push-token` require the
  target to be the caller's own device (`require_own_device`), else `403 device_mismatch` before
  any lookup. No IP binding: a session can roam and refresh remotely.
- **Tailnet machine binding.** The `pondnet` bridge asks the tailnet which machine sent each request
  (`WhoIs`) and forwards its machine key as `X-Pond-Embedded-Machine`, replacing any the client sent;
  a peer it cannot name gets `503 embedded_peer_unknown`. `trusted_peer` refuses a tailnet request
  without one. The auth middleware then accepts a token over the tailnet only from the machine its
  device enrolled (`RemoteMachines::machine_for`), else `403`. The Pond records that machine when an
  enrollment succeeds (`embedded-network/machines.json`), forgets it when the device enrolls again,
  recovers or is revoked, and otherwise asks the coordinator (`inspect`) once; an unreachable
  coordinator answers `503`, never lets the request through. LAN and loopback requests are unaffected.
- **Presence lapse.** A device renews remote access by authenticating from a directly attached LAN
  peer on the companion listener (`DevicePresence::seen_on_lan`, written at most once an hour per
  device to `embedded-network/presence.json`). After `LAN_PRESENCE_WINDOW_DAYS` (30) without that,
  its remote access is revoked through the same queue. The tailnet router is built before presence
  is layered, so a tailnet request never counts as home. Devices never sighted, and households that
  never enabled remote access, are never swept. The queue (`embedded-network/revocations.json`,
  `embedded_network/revocation.rs`) backs each entry off from 30 s doubling to 6 h. Details:
  [remote access](remote-access.md#disabling-removal-and-revocation).

This is bearer authorization, not proof of possession of a device key. A stolen session acts as
its device on the household LAN until expiry or revocation; over the tailnet it works only from
the device's enrolled machine; an open SSE stream is not re-checked per event, though new
requests with a revoked token fail at once. Clearing a client's local credentials does not revoke
anything on the server.

### Administration (My Pond)

`/api/v1/admin/*` needs more than a token. The caller must be a member's device, and that device
must have unlocked My Pond in the last 15 minutes, with the admin card or with the key the Pond
issued that device. A phone keeps its key behind its own biometrics; the phone that claimed the
Pond gets one when owner setup completes, and rotating the card revokes them all. The unlock is held in memory
per device, so a restart or a rotated card locks every phone. The card itself is checked against a
verifier in `<data_dir>/setup/admin.json`. Wrong cards are slowed as the setup code is, across
every client. Design, recovery and the wire contract are in the
[pond administrator](architecture/pond-administrator.md) note.

### Bootstrap answers

- `GET /health` answers `{status, version}` to a loopback peer (not on the plaintext listener) and
  `{status}` to everyone else.
- `GET /system/info` without a valid bearer returns only `https_port`, `tailnet_address` and
  `protocol: 3`; with one, `hostname`, `lan_address`, `tailnet_address`, `https_port`,
  `tls_spki_sha256`, `port`, `version`, `platform` and `arch`. While the plaintext listener runs,
  both carry `insecure_dev: true` and the anonymous answer adds the pin.
- The pin and addresses for the QR come with the code from the host-only `/handshake/pairing-code`
  (`pairing`). mDNS advertises `_pond._tcp` with TXT `v`, `scheme`, `pin`, `id` and, while
  unclaimed, `setup=1` (`PondAdvert`, `crates/pond-core/src/network/domain/pond_advert.rs`).
  Discovery supplies candidates, never trust.

## Pairing

Every pairing route, the setup code's `pake/init` and `POST /handshake/spoken-code` included,
requires a loopback peer or one inside an active, directly attached LAN interface's netmask
(`crates/pond-api/src/network.rs::require_lan`). Tailnet addresses, point-to-point and down
interfaces, and interfaces named as tunnels, overlays or container bridges (`tailscale`, `utun`,
`tun`, `tap`, `wg`, `wt`, `zt`, `feth`, `ham`, `nebula`, `ipsec`, `gif`, `stf`, `ppp`, `docker`,
`veth`, `virbr`; `NON_LAN_INTERFACE_PREFIXES` in `pond-core/src/network/domain/interface.rs`) do
not count. Missing metadata or a failed interface read is denied. The refusal is
`403 pairing_requires_lan`. Refresh works from anywhere.

| Secret | Lifetime | Retired | Used by |
|---|---|---|---|
| Pairing code, six digits | 10 min, single use; issuing one retires every older one | after `PAKE_FAILURE_BUDGET` (10) failed confirmations | phones (CPace), the dashboard (HMAC) |
| Setup code, eleven digits | the card's life | never; guessing is slowed instead | the first phone, while unclaimed |
| Spoken code | an ordinary pairing code | as a pairing code | a phone in earshot |
| Refresh hash (`credential: "device"`) | the refresh token's | on rotation | the BLE setup channel only ([setup channel](architecture/pond-setup-channel.md)) |

### Phones: CPace

Phones prove the code with CPace (`draft-irtf-cfrg-cpace`, suite `CPACE-RISTR255-SHA512`;
`crates/pond-infra/src/pairing_cpace.rs` for the math, `sqlite_handshake.rs` for state):
`POST /handshake/pake/init {client_id, …, channel_binding, client_nonce, credential?}` returns the
Pond's share; `POST /handshake/pake/verify {challenge_id, share, confirm}` checks the tag, consumes
the code, mints tokens and only then returns `server_proof`, which the phone checks before keeping
them.

- **No offline test.** Nothing in a run can be checked against a code offline; each guess costs a
  live `verify` against this Pond. Passive observers learn nothing about the code.
- **Channel binding.** The channel identifier carries the Pond's own TLS pin
  (`goose-pair-cpace-v1`; `goose-setup-cpace-v1` for the setup code; `goose-device-cpace-v1` for a
  resume), so a phone that pinned an interceptor's key fails confirmation when relayed. The phone
  also names its pin in `init`; a mismatch is `403 channel_binding_mismatch`, an early clear
  failure only. CPace does not authenticate the Pond's key: tokens go to whoever holds the pinned
  key, so the pin must be right. The QR, which carries the pin from the Pond itself, is the
  authentic source; the mDNS `pin` is safe to offer because a wrong one ends in a visible failure.
- **Budget.** Each code allows 10 confirmations, reserved before the tag is checked so concurrent
  verifies cannot exceed it; the attempt that spends it answers `code_retired` and ends every run
  committed to the code. That caps an attacker at 10 guesses per code whatever addresses it holds.
  The cost is that anyone on the Wi-Fi can burn the code on screen; that shows as `code_retired`
  and a debounced "Failed pairing attempt" alert, and the operator issues a new code. Failure
  counts live in memory, like the code's plaintext.
- **Runs.** A run (`challenge_id`) is removed before it is checked, so a failed verify cannot be
  retried on it. Runs live 60 s, in memory only (the Pond's scalar is zeroized), at most 256 at once
  (`503 pairing_busy`).
- **`init` refusals.** `409 no_active_pairing_code`, `409 setup_code_unavailable`,
  `400 invalid_client_nonce`, `400 invalid_credential` (an unknown credential, or `"device"` over
  HTTPS), `503 pairing_busy`, `429 setup_code_cooling` with `retry_after` and `Retry-After`.
  A refused verify is `200 {accepted: false, error, rejection_reason}`.
- **Audit.** Outcomes are `auth.device_paired` / `auth.pairing_verify_failed` with `method`
  `cpace`, `cpace-setup` or `loopback_hmac`, and `transport = "insecure_dev"` on the plaintext
  listener. No code, MAC or token is ever logged.

### The dashboard: loopback HMAC

The dashboard reads the plaintext code from the host-only `/handshake/pairing-code` and pairs
with `POST /handshake/init` then `POST /handshake/verify {challenge_id, mac}`,
`mac = HMAC-SHA256(code, challenge || client_id)`. A PAKE would protect a secret the dashboard
was just handed, and loopback has no middle. Any non-loopback peer, and any peer on the plaintext
listener, gets `403 pairing_requires_cpace`. The single-step `POST /handshake`, which carries the
code in its body, refuses every non-loopback peer with `403 legacy_pairing_host_only`. These have
no failure budget; the verify rate limit applies.

**A pairing tells the household, unless it is the dashboard re-pairing itself.** An accepted
pairing pushes "New device paired" to the paired phones (`emit_pairing_outcome`). The dashboard
re-pairs over loopback HMAC whenever a reload finds its stored session gone, under the client id
it keeps in `localStorage`, so before 2026-10-23 every such reload told the phone a device had been
paired when the devices list still held one "Pond Desktop" row. The adapter now reports whether a
pairing re-admitted a row it already had under the same client type
(`HandshakeResponse::returning_device`, never serialised), and `announces_pairing` quiets the
notice only for that case on `loopback_hmac`, whose code comes from the host-only pairing-code
route. A CPace pairing under a known id still notifies: the id is self-reported over the LAN, and
such a pairing takes the row over and revokes that device's sessions. Phones that already paired
come back through `/handshake/refresh`, which never notifies. Every pairing is still an
`auth.device_paired` event; a return carries `returning_device = true`.

### A Pond with no screen

Design and wire contract: [pond networking](architecture/pond-networking.md#claiming-a-pond).

- **Setup code.** Ten random digits and a Luhn check digit, printed on a card by
  `pond-server setup-code --card <path>` (at `giap.sh install`, or fetched by `giap.sh provision`).
  Stored in `<data>/setup/setup.json` (`0600` in a `0700` directory), never in `Settings` or the
  database. A phone sends `"credential": "setup"` in `pake/init`. It is accepted only while the
  Pond is unclaimed (`409 setup_code_unavailable` after). It is never retired, since anyone in
  range could then destroy the card: five wrong attempts are free, then one per 30 s doubling to
  one per 10 min, across every client, held in memory (`SetupGuessThrottle`).
- **The claim latch.** A confirmed setup run writes `claimed_at` before any token exists; if
  another device won in between it mints nothing. Every pairing-code pairing latches it too, best
  effort (`kind="setup_claim_latch_failed"`). `pond-server setup-code --rearm` clears it; paired
  devices keep working. A Pond that already had paired devices when its record was made is created
  claimed.
- **Spoken code.** `POST /handshake/spoken-code` issues an ordinary pairing code with the host's
  default attribution and reads it aloud twice; the answer is `202 {spoken, expires_at}` and never
  carries the code. At most one per 30 s and ten an hour, whatever asks (`429 spoken_code_limited`).
  It refuses with `503 announcer_unavailable` before issuing when the Pond cannot make a sound,
  since issuing retires the code on any screen. `spoken_pairing_codes` (on by default) turns it off
  (`403 spoken_codes_disabled`). Audited as `auth.spoken_code_issued`, without the code.

Neither code appears in a log line, event attribute or REST response; only
`pond-server setup-code` output and the card file show the setup code.

## CORS

`allowed_origins()` in `crates/pond-api/src/lib.rs`: `app://giap` (the packaged desktop
renderer), in debug builds also `http://localhost:1420` and `http://127.0.0.1:1420` (Vite), in any
build the origin `GIAP_DEV_SERVER` names, and any comma-separated origins in
`POND_CORS_ALLOWED_ORIGINS`. Never `Any`. The same list feeds the dashboard's Origin check and the
remote-access management router. Allowed headers: `Authorization`, `Content-Type`,
`X-Pond-Host-Credential`.

The renderer is served from `app://giap` rather than `file://` because a `file://` page sends
`Origin: null`, which cannot be allow-listed meaningfully. Native clients (goose-on-the-go) send
no `Origin`, so CORS does not apply to them.

## Rate limiting

| Bucket | Limit | Covers | Loopback |
|---|---|---|---|
| General | 600 / 60 s per IP | everything except `/api/v1/handshake*` | exempt |
| Pairing | 30 / 60 s per IP | every `/api/v1/handshake*` route, `spoken-code` included | exempt |
| Verify | 10 / 60 s per IP, shared by both verify routes | `/handshake/pake/verify`, `/handshake/verify` | **not exempt** |

General and pairing are mutually exclusive (`lib.rs::rate_limit_with_limiter`, `is_handshake_path`)
and answer through `AuthError::RateLimitExceeded` with a `Retry-After` header. Pairing has its own
budget so a chatty client cannot lock a device out of `/handshake`, the recovery path. Verify is
checked in the handler (`routes.rs::check_verify_limit`) and answers
`429 {"error": "pairing_busy", "retry_after_secs": N}` with no header; a pairing client must read
both shapes. The per-code failure budget bounds guessing absolutely (10 per code, about 0.001% of
the key space); the per-IP buckets keep one source from burning codes or flooding runs.

## Development switches

Each weakens a boundary above. None is on by default; none belongs on a household's Pond.

| Variable | Effect | Release binary |
|---|---|---|
| `POND_DEV_ALLOW_LOOPBACK=1` (or `true`) | Tokenless loopback requests pass the auth middleware with a loopback principal, and a missing host credential is accepted. The Host and Origin checks still apply. | Honoured |
| `POND_DEV_INSECURE_LAN=1` | Opens the plaintext companion listener (below). | Ignored, with `kind="insecure_dev_lan_ignored"` |
| `POND_DEV_INSECURE_LAN_PORT` | Its port; default 4080, bound exactly, no fallback. | Ignored with it |
| `POND_DEV_SAME_MACHINE_MESH=1` | Mesh dials allocate a new port so two Ponds can run on one machine. | Honoured |
| `POND_NETWORK_BINARY` | Path of the embedded-networking helper instead of the bundled `pondnet`. | Ignored |

### The plaintext development listener

Expo Go has no native module to pin the Pond's key, so it cannot use the HTTPS listener. A debug
build started with `POND_DEV_INSECURE_LAN=1` serves the companion API over plain HTTP on
`0.0.0.0:4080` (`crates/pond-api/src/insecure_dev.rs::decide`, which needs both the variable and
`cfg(debug_assertions)`). In a debug build a value other than `1`, `0` or empty, or a bad port,
stops startup.

Everything on it is plaintext and unpinned: anyone on the network can read every request and
response including tokens, replay those tokens against the HTTPS listener, and impersonate either
side. CPace keeps the code from being recovered offline, but the tokens it mints still cross in
plaintext. Use it on a network you control, with a Pond holding nothing you mind losing, and
re-pair over HTTPS afterwards.

It serves the companion router with the same auth, rate limits, LAN pairing guard and revocation
hook, never the dashboard, remote-access management or enrollment, and never presence. It refuses
browser requests (`Origin` or `Sec-Fetch-Site` present: `403 browser_request_refused`) and a
`Host` that is not an IP literal, `localhost` or this machine's name (`421 host_not_allowed`).
Each request carries the `InsecureDevTransport` extension, set by the router, so a loopback peer on
it is never the host. mDNS does not advertise it. While it runs, `/system/info` reports
`insecure_dev: true` on every listener, and the process logs `kind="insecure_dev_lan"` at start and
every ten minutes.

## Residual risks

- A stolen bearer token works from the household LAN until expiry or revocation; refresh is
  possession-based with a 30-day window. Over the tailnet it works only from the enrolled machine.
- A Pond host that also runs system Tailscale can receive tailnet traffic on the companion listener
  directly, bypassing the bridge and its machine binding. Run remote access through `pondnet` only.
- A compromised tailnet member can reach the public routes above.
- Native Android and iOS pin the Pond's key; browsers do not. Source-masking proxies and subnet
  routers cannot establish that a pairing peer is local and are unsupported.
- A compromised Pond OS or phone exposes its own keys and credentials; pinning cannot protect an
  endpoint from itself.
