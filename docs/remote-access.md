# Remote access with pinned HTTPS

How a paired phone reaches the Pond at home and away, what the Pond runs to make that
work, and what an operator must back up. The authentication contract (bearer tokens,
pairing, host credential, presence lapse) is in
[auth-network-posture.md](auth-network-posture.md); the coordination service is in
[the deployment guide](../deploy/remote-access/README.md); the Go module is
[native/pondnet](../native/pondnet/README.md).

## Connection model

The companion app (goose-on-the-go) pairs on the Pond's directly attached LAN. Once
paired it uses HTTPS on the LAN at home and the same HTTPS inside an embedded
WireGuard tunnel away from home. TLS terminates on the Pond; no cloud service holds
its private key.

Both ends embed a userspace Tailscale node (tsnet, `tailscale.com` v1.104.2 pinned in
`native/pondnet/go.mod`, built with Go 1.27.3). v1.104.2 and Go 1.27.3 are the floor:
earlier builds trust the leaked eMudhra intermediates (TS-2026-013, CVE-2026-111943,
GO-2026-6809) when the node fetches the coordinator's key over HTTPS. The Pond runs it as the supervised helper `pondnet`; the
phone links it through gomobile bindings. No Tailscale app, OS VPN profile or Tailscale
account is involved. A profile left local-only (**Keep local only**) starts no node and
contacts no coordinator.

Remote operation talks only to the configured Headscale coordinator and its DERP
relay. A household that names none gets the hosted pair, `https://controlpond.jarida.io`
and `https://enrollpond.jarida.io` (`DEFAULT_CONTROL_URL` / `DEFAULT_ENROLLMENT_URL` in
`crates/pond-server/src/embedded_network.rs`), applied when remote access is enabled and
never on read, so an empty control URL keeps meaning local-only. A household running its
own service sets both origins under **Use my own coordination service** on the
dashboard; setting only one is rejected. One exception to "only the configured
endpoints": when system DNS cannot resolve the coordinator, tsnet may ask Tailscale's
DERP servers to resolve the name (Residual risks in the deployment guide). That reveals
the client's address and the coordinator's host name, nothing else.

Default-deny coordinator policy lets an approved phone reach only its own Pond's HTTPS
port. Source-masking proxies and subnet routers are unsupported for pairing: listeners
classify the actual TCP peer and never trust forwarding headers. Do not publish the
companion listener on the public internet.

## Listeners

| Listener | Binds | Serves |
|---|---|---|
| HTTP | `127.0.0.1`, default 4000 (`serve --port`) | dashboard, desktop, CLI, OAuth callbacks, remote-access management |
| HTTPS | `0.0.0.0`, default 4443 (`serve --https-port`) | companion API, LAN pairing, phone enrollment |
| Tailnet | helper's tsnet listener on the HTTPS port | `/api/v1/` only, forwarded over a private Unix socket |
| Plaintext dev | `0.0.0.0:4080`, debug builds with `POND_DEV_INSECURE_LAN=1` only | companion API for Expo Go; never remote access |

Each of the first two tries ten consecutive ports (`ports::bind_with_fallback`); read
`<data_dir>/.runtime_api_port` and `.runtime_https_port` for the bound ones. Neither
falls back to plaintext. The plaintext dev listener is described under "Development
switches" in [auth-network-posture.md](auth-network-posture.md).

The tailnet listener is the helper's (`ServePond`, `native/pondnet/bridge.go`). It
terminates TLS with the Pond's key and forwards to a private Unix socket
(`pond-net-*/api.sock`, `0600` in a `0700` temp directory). Only the router behind that
socket accepts the helper's `x-pond-embedded-peer` header, so the API sees the real
tailnet peer; such a peer is remote for every LAN-only and host-only guard.

`GET /api/v1/system/info` gives an anonymous caller only `https_port`,
`tailnet_address` and `protocol`; the pin comes with the pairing code from the host-only
`/handshake/pairing-code`, and from mDNS. mDNS advertises `_pond._tcp` with TXT `v`,
`scheme=https`, `pin`, the setup `id`, and `setup=1` while the Pond is unclaimed
(`PondAdvert::txt`, `crates/pond-core/src/network/domain/pond_advert.rs`). Discovery
supplies candidates, never trust.

To use the dashboard from another computer, tunnel to the loopback port
(`ssh -L 9000:localhost:4000 <pond>`), run `pond-server dashboard` on the Pond and open
the link it prints with the port changed. The link carries the host credential after
`#` and stops working at the next restart. A tunnel is administrative access, not a way
to pair phones.

## Pairing and the pin

The dashboard's Pairing page and `pond-server pairing` show the same QR
(`tls_identity::pairing_url`, `pond-desktop/src/sections/Pairing.tsx`):

```text
pond://pair?v=2&scheme=https&host=<hostname>.local&port=<https-port>&code=<6 digits>&pin=<url-encoded sha256/base64-SPKI>&ip=<LAN address>&ts=<tailnet address>
```

`ip` and `ts` are optional. The pin hashes the DER SubjectPublicKeyInfo, not the
certificate. Manual pairing needs the HTTPS address, the full `sha256/...` pin and the
code shown locally. A Pond with no screen is claimed with its printed setup card
(`pond://setup?v=1&code=<11 digits>&id=<4 hex>`, from `pond-server setup-code`) or, once
claimed, asked to read a pairing code aloud (`POST /handshake/spoken-code`).

Phones pair with CPace (`/handshake/pake/init`, `/handshake/pake/verify`); the HMAC
exchange (`/handshake/init`, `/handshake/verify`) and the single-step `/handshake` are
for the host alone. Every pairing route requires a loopback or directly attached LAN
peer and answers anyone else `403 pairing_requires_lan`, so a run started at home
cannot be completed over the tailnet. Refresh and every other authenticated route work
remotely. The protocol, refusal codes and failure budget are in
[auth-network-posture.md](auth-network-posture.md).

## The Pond's certificates

`tls/identity.json` under the data directory holds one ECDSA P-256 key and its
self-signed certificate (`crates/pond-server/src/tls_identity.rs`). The directory is
`0700`, the file `0600`, and `identity.lock` keeps a second writer out. A missing file in
an existing directory, invalid JSON, a mismatched key, loose permissions or a symlink
stop startup. Restore the bundle from backup; never delete it to silence the error.

The key persists. Certificates are valid for a year and renewed within 30 days of
expiry or when the address SANs change; a 30-second loop in `serve` checks and reloads
rustls without changing the pin. Replacing the key means re-pairing every phone.

`tls/tailnet-identity.json` is a second certificate from the same key, served only by
the helper. Its SANs are the node's tailnet addresses and its node name,
`goose-in-a-pond` (`TAILNET_HOSTNAME`), so a tailnet peer does not learn the host name
or LAN addresses. `serve` writes it at startup if missing (a failure logs
`kind="tailnet_certificate_unavailable"` and only remote access waits on it), the same
loop renews it when the node's addresses change or expiry nears, and one that is
damaged or carries another key is replaced with `kind="tailnet_certificate_replaced"`.
The helper re-reads it on every handshake. An address is published to phones only once
this certificate names it. The LAN certificate still lists any tailnet address on the
host, because a system Tailscale reaches the LAN listener directly.

## Enabling remote access on the Pond

The Remote access section of the dashboard's Pairing page drives the loopback
management routes (`embedded_network::management`), which need a loopback peer and the
host credential:

| Route | Purpose |
|---|---|
| `GET/POST/DELETE /api/v1/remote-access` | status, enable, disable |
| `POST /api/v1/remote-access/identity` | create or load the household authority (**Prepare household identity**) |
| `POST /api/v1/remote-access/register` | register the household and enroll the Pond's own node |
| `GET /api/v1/remote-access/device` | `{provisioned, serial?, registered}` |
| `GET /api/v1/remote-access/recovery-requests`, `POST .../{id}` | review and approve phone recovery |

**The household authority** is an Ed25519 key in `embedded-network/authority/`. Register
creates it on first use and loads it afterwards. A directory that exists without
`identity.json` is refused as a lost key (`authority identity is missing; restore its
backup`), never replaced.

**Admission.** A household's first registration needs either an invite from whoever runs
the coordination service or a device certificate:

- *Invite.* The dashboard shows an invite field; the Pond passes the invite to the helper
  on stdin (`--authority-action register`) and never stores it.
- *Device certificate.* `bash scripts/giap.sh provision --host HOST --key FILE`
  (`scripts/provision-device.sh`), run on the operator's machine, saves the Pond's setup
  card, creates a device key on the Pond in `embedded-network/device`, signs its public
  half with the operator's offline provisioning key and installs the certificate. Its
  defaults assume the Jetson layout (`~/.local/share/goose-in-a-pond`,
  `~/goose-in-a-pond/target/release/{pondnet,pond-server}`); pass `--data-dir`,
  `--pondnet` and `--pond-server` for any other Pond. It needs `ssh` and `python3`, and
  builds `pond-provision` from `native/pondnet` (needs Go) when it is not on `PATH`. The
  dashboard then shows the device serial, which is what an operator revokes a lost Pond
  by.

Once registration succeeds, or the Pond's node first reaches `Running`, the Pond writes
`embedded-network/registered.json` naming the enrollment origin. `GET .../device`
reports `registered` only for the origin configured now, so moving to another service
asks again. The dashboard shows the invite field only to a Pond that is neither
provisioned nor registered, and after any admission refusal (`ADMISSION_REFUSALS`,
`pond-desktop/src/sections/RemoteAccess.tsx`); it re-reads the device status after enable,
disable and every refusal.

`POST /remote-access/register` takes an optional `{"invite": string}` and refuses unknown
fields with `422`. It answers:

| Status | Error | When |
|---|---|---|
| 403 | `host_only` | not a loopback peer |
| 400 | `invite_invalid` | a string that could never be an invite (checked before the helper runs) |
| 409 | `no_pending_registration` | the node has no pending registration |
| 403 | `invite_required`, `invite_invalid`, `invite_expired`, `invite_used`, `device_certificate_invalid`, `device_revoked`, `device_used` | the coordinator's admission refusal |
| 503 | `registration_unavailable` | any other registration failure |
| 503 | `enrollment_unavailable` | enrolling the Pond's own node failed |

## Phone enrollment

The phone calls the companion routes (`embedded_network::companion_management`), which
take a valid bearer, a 4 KiB body and 20 requests a minute:

| Route | Purpose |
|---|---|
| `GET /api/v1/remote-access/configuration` | `{enabled, controlUrl, state, lapsesAt, enableFrom}`; while off, `enableFrom` names where the household can turn it on (`["dashboard"]`), so the app can explain it once the phone is away |
| `POST /api/v1/remote-access/enrollment` | enroll this phone's node (LAN peer only) |
| `POST /api/v1/remote-access/recovery`, `GET/POST/DELETE .../recovery/{id}` | request, poll, execute and cancel a recovery (LAN peer only) |

`register_phone` records the device in `enrolled.json` before asking the helper, then
inspects the coordinator's record and decides (`phone_enrollment`):

| Coordinator record | Pond does | Phone gets |
|---|---|---|
| `active`, same machine key | nothing | the existing enrollment (`200`) |
| `revoked` or `failed`, same machine key | sends nothing; the coordinator would refuse the key | `409 {"error":"stale_identity"}`; the app makes fresh node keys and enables again |
| `revoked`, other key | `replace` at the reported revision | `200` |
| none, `active` with another key, or `failed` with another key | `enroll` | `200`, or `409` when the coordinator keeps the existing enrollment (`invalid_enrollment`), which sends the phone to recovery |

A refusal for `retired_capacity` answers `503 {"error":"retired_capacity"}` (`PhoneRefusal`),
since recovery would meet the same bound. Every other refusal or outage answers `503`.
An expected `409` logs at `INFO` (`kind="remote_access_recovery_required"`,
`kind="remote_access_stale_identity"`); faults log `embedded enrollment failed` at `WARN`.

The helper exits 3 when the coordinator refused and 1 when it could not be reached; on
exit 3 it prints `{"refused": "<identifier>"}` and the Pond keeps the coordinator's
reason and the helper's redacted stderr. A device recorded only for this call is
forgotten again when the coordinator refused before writing anything (the reasons in
`refused_before_writing`); any other failure keeps the record, so a later removal revokes
unchecked, which is the safe way to be wrong. A revocation queued while the call was in
flight wins: the phone gets `409`, not a success the revocation is about to undo.

### Recovery

Recovery replaces an `active` enrollment held by another identity, typically a phone
that re-paired and regenerated its node keys. The phone, on the LAN with a valid bearer,
posts a recovery request; the Pond inspects the record and refuses (`409`) when the phone
already holds the enrolled key or the record has no usable revision. The request waits
120 s for approval. The dashboard's **Phone recovery requests** section appears only
while one is waiting or the list cannot be read, polls every five seconds, and names the
phone from the device list with the first sixteen hex digits of the replacement's
machine key, which the phone shows too. After approval the phone, re-authenticated and
still on the LAN, executes it: the Pond revokes the active enrollment, re-reads the
revision from that answer and replaces. Nothing is revoked at request time, because that
route needs only a LAN peer and a token. An approved recovery the coordinator refuses
(for example `retired_capacity`) stays on the review for ten minutes with the reason.

## Disabling, removal and revocation

Disabling stops the node and keeps the pairing. Sign-out (`POST /handshake/revoke`) and
removal (`DELETE /devices/{id}`) queue the device's network revocation durably before
revoking its sessions; the details are in
[auth-network-posture.md](auth-network-posture.md), as is the 30-day presence lapse.

Files under `<data_dir>/embedded-network/` (`embedded_network/revocation.rs`):

- `revocations.json` is the durable queue. Each entry backs off on its own, from 30 s
  doubling to six hours, four entries per 30-second tick; one device the coordinator keeps
  refusing does not hold up the rest. An entry refused with `enrollment_missing` is
  dropped. An unreadable queue is refused, never discarded.
- `enrolled.json` lists the phones this Pond enrolled, at most 256. A listed phone is
  revoked outright; when the record is complete an unlisted device is not sent at all,
  since a revocation of a device the coordinator never saw leaves a tombstone. An
  unreadable record is moved aside as `enrolled.json.corrupt-<unix time>`
  (`kind="enrollment_record_replaced"`) and read as incomplete, so removals check with
  the coordinator first.
- With an enrollment origin configured but no authority, every coordinator call fails.
  A listed phone's revocation is still queued and backs off until the authority is
  restored; one that would need a check first is not queued, and such an entry already
  queued is dropped (`kind="remote_revocation_without_authority"`).
- `presence.json` holds LAN sightings for the presence lapse.

An empty queue is written as `[]`, the form binaries without per-entry backoff read.
Before installing such an older binary, let the queue drain until
`revocations.json` reads `[]` or is absent, with the coordinator reachable.

## The helper

`serve` runs `pondnet` from beside its own executable; `POND_NETWORK_BINARY` overrides
that in debug builds only. The helper must be a regular file owned by the Pond's account
or root and writable by nobody else, or remote access does not start and the log names
its path; `scripts/build-network-helper.sh` builds it with mode `0755`. It runs with an
empty environment except `HOME`, `TMPDIR`, `SSL_CERT_FILE` and `SSL_CERT_DIR`, so an
inherited `TS_AUTHKEY` or `HTTPS_PROXY` cannot join it to another tailnet or route its
coordinator traffic. Each call is bounded at 25 s, and calls for one device are
serialised without blocking other devices. Node and machine keys are checked for their
exact shape before anything is signed for them.

The Pond's node never runs from a cached network map: `cmd/pondnet` sets
`TS_USE_CACHED_NETMAP=false`, so the packet filter that decides which phones reach the
Pond is always the coordinator's live one.

## Phone side

The companion app's transport (endpoint selection and roaming, SecureStore profiles,
the Android and iOS network layers that enforce the pin, hostname and validity before
any Pond request, ATS exceptions for `100.64.0.0/10` and `fd7a:115c:a1e0::/48`) lives in
goose-on-the-go and is not verifiable from this repository (unverified here). On the
phone, native code gets an authenticated loopback CONNECT proxy from
`mobile.ProxyConfiguration` that tunnels only to the paired Pond's tailnet addresses
(`mobile.SetTargets`); see [native/pondnet](../native/pondnet/README.md). The parts
below are in `native/pondnet/mobile` too.

**Resolver.** The phone's node does not pick a resolver or a transport: every query goes
to every resolver the OS reports, over UDP and TCP at once, and the first usable answer
carrying the query's id wins (`dialResolver`, `native/pondnet/mobile/dns.go`). A TCP
answer counts once complete; a truncated UDP answer is used only if nothing better
arrives. FORMERR, SERVFAIL, NOTIMP, REFUSED and lame replies (`unusableReply`) are held
back so a fast failing server cannot beat a slower good one; NXDOMAIN and NOERROR end the
race. A race every server loses ends at once with one event-log line. The networks that
forced each rule are recorded in
[the verification ledger](embedded-remote-access-verification.md).

**Node directories.** Each pairing's node lives in its own directory (Android
`noBackupFilesDir/pond-network/<profile>`, iOS Application Support
`pond-network/<profile>`), holding its keys in `tailscaled.state`. `mobile.Prune(base,
keep)` erases every other node directory: only entries with a valid profile name that
`Lstat` finds to be a directory holding `node.lock`, `initialized` or `tailscaled.state`,
or nothing, and never one whose node lock is held. Links and anything else are left in
place. goose-on-the-go's native plugins call it when a profile activates.

**Cached network map.** A phone's node keeps its last network map in
`<state>/profile-data/<profile>/netmap-cache/` (`0600` files in a `0700` directory, no
private keys; asserted in `native/pondnet/netmapcache_test.go`) and starts from it at a
cold start instead of waiting for the coordinator. tailscale writes the cache only for a
node granted the `cache-network-maps` node attribute, which the enrollment service grants
to every active, verified phone's address and never a Pond's. A stale cached map cannot
widen access, because the Pond enforces from a live map.

- `mobile.Start` runs `Node.ForgetNetworkMapWhenRefused`: when the coordinator answers
  with a login URL (the device was removed), the cache is cleared and removed, the
  identity kept, and one event-log line written.
- `mobile.Disable(directory)` asks the backend to clear the cache, stops the node, then
  removes the cache directories of both the given directory and the one the node actually
  ran in (`cacheDirectories`), returning every failure. `mobile.Stop` keeps the cache.
- `TS_USE_CACHED_NETMAP=false` in a node's environment turns the cache off.

## Backing up the Pond

Trust is the combination of three items, restored together:

- the household authority, `embedded-network/authority/identity.json` (losing it means a
  new household and re-pairing every phone; there is no cloud recovery),
- the HTTPS identity, `tls/identity.json` (`tls/tailnet-identity.json` is rebuilt from it),
- the node state, `embedded-network/node/tailscaled.state`.

`scripts/pond-snapshot.py` streams a tar of the Pond's irreplaceable state to stdout:
`embedded-network/`, `tls/`, `secrets/`, `secrets.json`, the schedule files, and
consistent copies of `pond_system.db` and `pond_vectors.db` through SQLite's online
backup API. It reads `~/.local/share/goose-in-a-pond` (the Linux layout). It skips
models, caches, binaries, logs, lock files and `embedded-network/device`: the device key
admits a household only on its first registration, so leaving it out keeps it off every
backup host. Run it from an operator machine and encrypt there:

```sh
ssh <pond> 'python3 -' < scripts/pond-snapshot.py | age -R <recipients> -o pond-state.tar.age
```

The copied databases carry a WAL header but no `-wal` file, so an explicit read-only open
(`file:...?mode=ro`) fails with `unable to open database file`. Check integrity with an
ordinary connection.

A Pond restored without `embedded-network/authority/` registers a new household when
remote access is enabled. Restore the authority first.

## Verification

Run the Go tests (`go test -race ./...` in `native/pondnet`), the Rust tests in
`embedded_network.rs`, and `scripts/live-test.sh` against scratch data including a
restart with populated databases. Physical acceptance needs real phones and a Pond: home
LAN, cellular, another Wi-Fi and home again, with app and server restarts, LAN address
changes, coordinator and relay outages, certificate renewal, and wrong-pin rejection on
REST and SSE, with any separate Tailscale app disconnected. Measured results and open
items are in [the verification ledger](embedded-remote-access-verification.md).
