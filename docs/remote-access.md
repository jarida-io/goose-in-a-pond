# Embedded remote access with pinned HTTPS

## Connection model

The Android and iOS companion pairs on the Pond's directly attached LAN. Once paired,
it uses HTTPS on the LAN at home and HTTPS inside Tailscale/WireGuard away from
home. HTTPS is HTTP over TLS; WireGuard is an additional encrypted transport.
TLS terminates on the Pond. No cloud service holds its private key.

GOTG embeds a userspace Tailscale/WireGuard node on Android and iOS. The Pond
bundles the same pinned Go networking module as a supervised helper. No separate
Tailscale app, system VPN profile, or user Tailscale account is required. After
local pairing, choose **Enable remote access** or **Keep local only**. Local-only
profiles do not start a node or contact a coordination service. Remote operation
uses only the explicitly configured Goose-operated Headscale and DERP endpoints, and
never a separate VPN app, with one exception: when system DNS cannot resolve the
coordinator, tsnet may ask Tailscale's DERP servers to resolve its name (see
Residual risks in [the deployment guide](../deploy/remote-access/README.md)). That
request reveals the client's address and the coordinator's host name and carries
no household data.

The first time a Pond joins a coordination service it needs an invite from whoever
runs that service (2026-09-30), pasted into **Remote access** on the dashboard. The
Pond hands it to the helper on stdin and never stores it; once the household is
registered no invite is needed again. The dashboard explains each refusal
(`invite_required`, `invite_invalid`, `invite_expired`, `invite_used`). Operators
issue invites as described in [the deployment guide](../deploy/remote-access/README.md).

A Pond the operator imaged needs no invite (2026-10-05). `scripts/giap.sh provision`,
run on the operator's machine, gives it a device key in `embedded-network/device` and a
certificate signed with the operator's offline provisioning key; its first registration
carries that certificate, bound to the household it registers, and the hosted service
admits it. `provision-device.sh` assumes the Jetson layout by default: data in
`~/.local/share/goose-in-a-pond` and the helper at
`~/goose-in-a-pond/target/release/pondnet`. Pass `--data-dir` and `--pondnet` for any
other Pond, including one on macOS. It needs `ssh` and `python3` on the operator's
machine, and builds `pond-provision` from `native/pondnet` (which needs Go) if it is
not on `PATH`. The dashboard then shows the device serial in place of the invite field,
which is what an operator revokes a lost Pond by. A certificate the service refuses
(`device_certificate_invalid`, `device_revoked`, `device_used`) brings the invite field
back, since an invite still admits the household.

A household already registered is not asked for an invite either (2026-10-05). Until then
the dashboard showed the field on every Pond nobody provisioned, including the Jetson,
which registered on 2026-09-20 and could never need one again. The Pond now writes
`embedded-network/registered.json`, private, naming the enrollment origin the household
registered with. It writes the record when registration succeeds and when its node first
reaches `Running`, which an enrolled Pond's node can do only after its household
registered; that is how a household that registered before the record existed gets one.
`GET /api/v1/remote-access/device` reports `registered` for the coordination service
configured now, so a household moving to another service is asked again. The dashboard
shows the field only to a Pond that is neither provisioned nor registered, or straight
after the service refuses an admission, and otherwise says why no invite is needed.

Any admission refusal brings the field back, on a provisioned Pond too (2026-10-12).
Before then only a refused certificate did, so two paths dead-ended: after
`device_used`, an invite typo answered `invite_invalid` and hid the field again, and a
coordinator with no provisioning keys answered `invite_required` to a provisioned Pond
that was never shown the field. The dashboard also reads the device status again after
enabling, after disabling and after a refusal, rather than once when the page opens, so
the field and the "already registered" note follow a registration made in the same
visit without a reload.

This branch prepares a locally tested pilot deployment. Public domains and hosting
are still prerequisites for cellular use. Follow
[the deployment guide](../deploy/remote-access/README.md) to provision a pilot
household and obtain local Pond approval. The enrollment service alone holds
Headscale administration credentials. The Pond signs short-lived, single-use
approvals binding its household, paired phone and pending network registration.
Replacement phones require local pairing and explicit approval; there is no cloud
account-recovery bypass.

Source-masking proxies and subnet routers are unsupported for pairing. Public
listeners classify the actual TCP peer, never forwarding headers. Embedded traffic
enters through a private Unix socket whose trusted helper supplies the real remote
peer identity. It remains remote for handshake and local-management guards. Do not
publish the companion listener directly on the public internet. Default-deny
coordinator policy permits an approved phone to reach only its own Pond HTTPS port.

## Listeners and local tools

- HTTP binds exclusively to `127.0.0.1`, normally port 4000. Desktop assets,
  development pages, CLI, and OAuth callback integration remain local.
- HTTPS binds to all IPv4 interfaces, normally port 4443, and serves the API.
  `serve --https-port PORT` overrides its starting port. Each listener tries ten
  consecutive ports. Neither listener falls back to plaintext network access.
- The one plaintext exception is development-only: a debug build started with
  `POND_DEV_INSECURE_LAN=1` also serves the companion API over HTTP on port 4080
  for the app running in Expo Go. Release builds ignore it, mDNS never advertises
  it, and remote access is unavailable through it. See
  [Development switches](auth-network-posture.md#development-switches-2026-10-05).
- Read `<data_dir>/.runtime_api_port` and `.runtime_https_port` for actual ports.
  `/api/v1/system/info` publishes `https_port` and `tailnet_address` to anyone; the
  pin comes with the pairing code from `/handshake/pairing-code` (host only) and
  from mDNS.
- mDNS `_pond._tcp.local.` publishes the HTTPS port and `scheme=https`, the pond's
  setup `id`, and `setup=1` while it is unclaimed.
  Discovery supplies candidates; it never supplies trusted keys.

To view the dashboard from another computer, use an authenticated SSH tunnel to
the loopback HTTP port. Preserve the existing OAuth redirect port. Such tunnels
are administrative access, not a supported remote mobile pairing mechanism.

A browser must be signed in before it can pair phones or manage remote access. On
the Pond, run `pond-server dashboard` and open the link it prints; through a tunnel
(`ssh -L 9000:localhost:4000 <pond>`), change only the port. The link carries the
host credential after `#`, is good until the server restarts, and should not be
shared. The desktop app needs no link. See
[the loopback listener](auth-network-posture.md#the-loopback-listener-2026-09-30).

## Pairing and identity

Open the local dashboard or run `pond-server pairing`, which prints the QR in the
terminal. A headless Pond no longer needs SSH to pair a phone: the phone scans the
Pond's printed setup card to claim it (`pond://setup?v=1&code=<11 digits>&id=<4 hex>`,
from `pond-server setup-code`), or, once the Pond is claimed, asks it to read a
pairing code aloud (`POST /handshake/spoken-code`) and types what it hears. Both run
the same CPace exchange on the LAN; see
[A pond with no screen](auth-network-posture.md#a-pond-with-no-screen-the-setup-code-and-the-spoken-code-2026-10-17).
The dashboard, CLI, and startup output use the same payload contract for a pairing code:

```text
pond://pair?v=2&scheme=https&host=<hostname>.local&port=<https-port>&code=<6-digits>&pin=<url-encoded-sha256/base64-SPKI>&ip=<LAN-address>&ts=<tailnet-address>
```

`ip` and `ts` are optional. The pin hashes DER SubjectPublicKeyInfo, not the
certificate. Manual pairing requires the HTTPS address, full `sha256/...` pin,
and pairing code displayed locally. Old HTTP-only profiles need a fresh local
scan; an unauthenticated HTTP response cannot establish trust.

The phone stages the pin before contacting the Pond. It probes only local
candidates for initial pairing. The server independently guards legacy handshake,
challenge initialization, and verification, returning 403 `pairing_requires_lan`
for tailnet or unclassifiable peers. A verify that omits `channel_binding` is
refused from any peer but loopback with 403 `channel_binding_required`, without
consuming the challenge; the legacy single-step `/handshake` answers 403
`legacy_pairing_host_only` off the host. Knowing a code or starting a challenge at
home does not permit completing it remotely. Code issuance remains loopback-only, except
that a direct LAN peer may have the Pond read a fresh code aloud.
Existing sessions and refresh tokens continue to work remotely.

Credentials and the pin are stored together in native SecureStore. Non-secret
addresses are stored separately and associated with that secure profile. Failed
pairing does not overwrite the previous secure identity. Cancellation clears
staged native trust and restores the previous profile.

## Certificate lifecycle

The Pond stores one atomic key/certificate bundle at `tls/identity.json` under
its data directory. The directory is mode 0700 and identity is mode 0600 on Unix.
An exclusive lifetime lock prevents competing writers. Missing material in an
existing directory, invalid JSON, a mismatched key, unsafe permissions, or a
symlink produces a visible startup failure. Restore the existing bundle from a
secure backup; do not delete it to silence an error.

The ECDSA P-256 key persists. Certificates are self-signed, valid for one year,
and renewed within 30 days of expiry or when current address SANs change. The
server checks every 30 seconds and reloads rustls without changing the pin. Key
replacement requires local re-pairing on each phone. This transport identity is
separate from any production signing key and requires no database migration.

The embedded helper serves a second certificate on the tailnet, `tls/tailnet-identity.json`
in the same `{cert_pem, key_pem}` format and with the same key (2026-10-12). It names only
the node's tailnet addresses and its node name, `goose-in-a-pond`, so a tailnet node no
longer learns the host name and LAN addresses from the certificate it is shown. It is
written at startup if missing (a failure is logged as `kind="tailnet_certificate_unavailable"`
and does not stop the Pond, but the helper cannot start without it), renewed by the same
30-second loop when the node's addresses change or expiry nears, and replaced, with a
`kind="tailnet_certificate_replaced"` warning, if it is damaged or carries another key; it
is derived from the key and holds nothing of its own. The helper re-reads it on every
handshake, and an address is published to phones only once this certificate names it.

## Roaming and failures

A shared manager selects endpoints for REST and foreground SSE. On Wi-Fi it
prefers pinned local addresses, uses mDNS to find a changed address, then tries
the authenticated embedded address when remote access is enabled. Cellular uses
the embedded node; local-only profiles remain disconnected away from home.
Network changes and foreground resume re-evaluate the endpoint choice; background
probing pauses. Recovery is coalesced, uses a capped backoff, and stops after six
failed attempts until another trigger or a manual retry. NetInfo does not perform
external reachability probes or collect SSIDs. On Wi-Fi, six bounded local
rechecks also allow a connected phone to return from tailnet after a temporary LAN
outage without waiting for a network-change event.

The node's resolvers are not chosen in advance, by server or by transport: every
query the node makes goes to every resolver the operating system reports, over UDP
and over TCP at once, and the first answer to that query is used
(`dialResolver`, `native/pondnet/mobile/dns.go`). Each narrower rule was defeated by
a network that was measured:

- **A home router, 2026-09-20.** Its routable resolver answered nothing; its
  link-local one answered only over TCP.
- **A home gateway, September 2026 (#394).** It refused DNS over TCP on both the
  IPv4 and IPv6 resolvers it advertised while answering UDP. This is the opposite
  of the 2026-09-20 measurement; both were measured, at different times, and the
  record does not say whether it was the same router in another state. Racing
  every query covers both.
- **Safaricom LTE.** The first resolver accepts a TCP connection in about 30 ms
  and never answers on it, while answering UDP in about 170 ms. Trying servers in
  order spent each lookup's budget on it, and taking a TCP connect as proof made
  the lookup hang on it.
- **The 2026-09-20 home router after a power cut, 2026-10-05.** A queries over UDP are
  answered in about 10 ms, but AAAA queries over UDP are never answered, from
  either of its addresses. AAAA is answered over TCP only, on the link-local
  address. The previous rule probed each server with one test query and used
  whichever transport answered for every later query. UDP won, so every lookup
  waited out its whole deadline for the AAAA answer. On the A57 the coordinator's
  name took 10.003 s to resolve, longer than tailscale allows for a control
  lookup, so a phone on that network could not enrol. Racing each query, the same
  lookup on the same phone takes about 30 ms.

A UDP answer counts only if it carries the query's id. A TCP answer counts only
once it is complete and carries that id. A truncated UDP answer is used only if
nothing better arrives. A race that every server and transport loses ends at
once, with one event-log line naming each failure, so Go moves straight to its
next attempt. Losers cancelled because another server answered are not reported.

A reply that says nothing about the name is held back the same way (2026-10-12,
#491). Until then the first reply carrying the id won whatever its RCODE, so one
server answering SERVFAIL fast failed a lookup that a slower server was about to
answer. FORMERR, SERVFAIL, NOTIMP and REFUSED are held back, and so is a lame
reply: NOERROR with no answer and no additional record from a server that
claims neither authority nor recursion, the test Go's own resolver applies
before it tries the next server (`errLameReferral` in `net/dnsclient_unix.go`).
A usable answer from any server still wins. Otherwise the race ends, once every
server has replied or failed, on the truncated answer if there is one, else on
the first held-back reply, passed to Go unchanged so it reports the server's
failure rather than a timeout, with one event-log line naming the RCODE.
NXDOMAIN and NOERROR, with or without records, are answers and end the race as
they arrive (`unusableReply`, `native/pondnet/mobile/dns.go`). A held-back reply
waits for every other attempt, so when no usable answer comes and some server
accepts TCP without ever answering on it, as Safaricom's first resolver does,
the lookup lasts until its deadline instead of ending on the held-back reply.
Before these changes, a cold start on cellular took fifteen seconds or more to
bring the node up, and sometimes it never came up: netcheck and the DERP
connection both need the coordinator's name resolved, under deadlines of about a
second.

Reads retry at most once after recovery. Writes and refresh-token rotations are
never replayed after ambiguous transport failures. A failed write may have
succeeded on the Pond; inspect the result before repeating it. Notification IDs
are deduplicated across stream reconnections.

The Android factory is installed before React Native and Expo initialization.
Both Expo fetch and React Native XHR use it. The configured pin, certificate
validity, and hostname must match before a Pond request is sent. Cross-origin
redirects and plaintext Pond URLs are rejected. Unrelated HTTPS traffic retains
platform trust validation. Release builds have no cleartext exception; debug
builds permit loopback HTTP for Metro through `adb reverse` only. Run Expo
prebuild to regenerate native integration from `plugins/with-pond-tls.js`.

On iOS, both Expo fetch and React Native XHR install the shared Pond URL protocol
before initialization. Configured Pond requests and every request to the embedded address ranges use this
protocol; unconfigured remote endpoints fail closed. Each accepted request uses an
ephemeral session with SPKI, validity, and hostname validation. Changing trust
cancels active requests, including SSE. Unrelated traffic uses normal platform
trust. Expo prebuild recreates the source files, bridge, and Xcode integration.
On iOS 17+, scoped ATS exceptions for `100.64.0.0/10` and
`fd7a:115c:a1e0::/48` allow native verification of the Pond's self-signed identity.
They do not replace native HTTPS, pin, hostname or date validation. Browser pinning
is outside this implementation. iOS 16.4 remains the build minimum, with its older
proxy path still awaiting runtime acceptance.

### Enabling, replacing and lapsing (2026-09-21)

Enabling remote access inspects the coordinator first and returns the existing
enrollment when the device is already **active and still holds the identity it
enrolled with**. Pressing the button on a pond where remote access already works
is a no-op rather than a conflict. A mismatched identity -- a phone that re-paired
and regenerated its tailnet keys -- is a real conflict while its enrollment is still
`active`: the enrollment is refused with `409` and the app points at recovery.
Because every re-pair of an active phone takes this path, the Pond logs it at `INFO`
as `kind="remote_access_recovery_required"`, not as a failure; `embedded enrollment
failed` at `WARN` is kept for refusals with no enrollment to recover, for a refused
replacement, and for a coordinator that could not be reached.

The household key is created on the Pond the first time remote access is enabled,
just before registration, and loaded on every later registration (2026-10-05).
Until then only the manual path's **Prepare household identity** created it, so
enabling remote access on a fresh Pond failed with `registration_unavailable`.

Recovery replaces an enrollment. The coordinator replaces one that has been stood
down rather than a live one, so the Pond revokes the existing enrollment itself
and then replaces it, and **only after a person has approved the replacement at
the Pond**. It is not done at request time: that route needs only a LAN peer and
a bearer token, so revoking there would let anyone with both drop the household's
remote access without approving anything. The revision is re-read from the
stand-down's own answer, because standing an enrollment down gives it a new one --
assuming otherwise cost a household its enrollment without a replacement. The
dashboard's **Phone recovery requests** section appears only while a request is
waiting, or when the requests cannot be read (2026-10-05); it polls every five
seconds, so a new request shows without a reload.

The review names what is being approved (2026-09-30): the phone's name from the
device list and the first sixteen hex digits of the replacement's machine key,
which the phone shows too. It used to show only a request id and a hashed device
id, so a household could approve whichever request arrived first without being
able to tell it was theirs.

The helper that holds the household's network identity runs with an empty
environment apart from `HOME`, `TMPDIR`, `SSL_CERT_FILE` and `SSL_CERT_DIR`, so an
inherited `TS_AUTHKEY` or `HTTPS_PROXY` cannot join it to another tailnet or route
its coordinator traffic. It must be a regular file owned by the Pond's account or
root and writable by no one else (`build-network-helper.sh` sets `0755`), and
`POND_NETWORK_BINARY` selects another helper only in debug builds. Node and machine
keys are checked for their exact shape before anything is signed for them.

A helper built before 2026-09-30 on a host with umask 002 (Ubuntu's default, the
Jetson's included) is group-writable and is now refused, so remote access does not
start; the log names the helper's path. Rebuild it with
`scripts/build-network-helper.sh`, or `chmod 0755` it.

A phone that was removed, or whose remote access lapsed, needs no recovery
(2026-10-05). Its enrollment is left `revoked`, and a re-paired phone keeps the
same device id. The coordinator refuses a plain enrollment over any existing
record, so on the Galaxy A57 a removed and re-paired phone got `409
invalid_enrollment` and could come back only through recovery: a second
dashboard approval after a pairing that had already needed one. Enabling remote
access now replaces a `revoked` enrollment at the revision the coordinator
reports, through the same `replace` the recovery uses
(`phone_enrollment` in `crates/pond-server/src/embedded_network.rs`). This grants
nothing a first enrollment does not: both need a LAN peer and a valid bearer, and
a stood-down record carries no working remote access to take over. An `active`
enrollment held by another identity is still refused and still needs the
approval above, and so does a `failed` one: every replacement retires the old
record against the household's bounded retirement budget, and a refused
enrollment can be failed again at will.

A lapsed or removed phone that was **not** re-paired still holds the machine key its
stood-down record keeps (2026-10-12). The coordinator keeps that key as a tombstone,
so a late registration under it can still be deleted, and refuses any `replace` or
`enroll` that presents it (`identity_already_enrolled`), whether the record is
`revoked` or `failed`. Recovery refuses it too, as there is nothing to replace. So
enabling remote access sends the coordinator nothing for such a phone and answers
`409 {"error":"stale_identity"}`, logged at `INFO` as
`kind="remote_access_stale_identity"`; the app answers by making fresh node keys and
enabling again, which replaces the record through the `replace` above (goose-on-the-go
holds that change). Once it lands, each return of a lapsed phone moves its old record
into the household's retired records, of which there are at most 256 shared by all its
phones.

That budget refills on its own since 2026-10-13. The coordinator drops a retired
record once its old machine key has stayed off the coordinator for 24 hours, so
it binds only a household that replaces 256 phones within about a day. If one
does, the coordinator refuses with `retired_capacity` instead of the
`replacement_conflict` a stale approval gets. The Pond answers the phone `503
{"error":"retired_capacity"}` (`PhoneRefusal` in
`crates/pond-server/src/embedded_network.rs`): a `409` would send it to recovery,
which meets the same bound. An approved recovery refused this way stays on the
dashboard's **Phone recovery requests** for ten minutes, marked with the reason
in place of its approve button, because the household that approved it would
otherwise see the request vanish with no outcome. A Pond older than this answers
`409` or `503` exactly as it did for `replacement_conflict`. The expiry rules
and why an expired key may enroll again are in `deploy/remote-access/README.md`.

Remote access lapses after thirty days without the device authenticating from the
household LAN; see `docs/auth-network-posture.md`. The deadline is reported in the
remote configuration and the app warns from a week out.

A failure reports which kind it is. The helper exits 3 when the coordinator
refused the request and 1 when it could not be reached, so `register_phone`
answers `409` for a decision and `503` for an outage, and the app can tell a
household whose phone is already enrolled from one whose coordinator is
unreachable. Household registration (`pondnet --authority-action register`) also
exits 3 on a coordinator refusal, and prints `{"refused": "<identifier>"}` on stdout.
Every layer carries the cause it was given: the Pond captures the
helper's stderr, the helper prints `Submit`'s error, and `Submit` carries the
coordinator's status and its error identifier. On exit 3 the helper also prints
`{"refused": "<identifier>"}` on stdout, the coordinator's own reason, so the Pond
can act on which refusal it was. The revocation queue drops an entry refused with
`enrollment_missing`, since there is nothing to revoke.

`POST /api/v1/remote-access/register` (loopback only, `403 host_only` otherwise) takes
an optional `{"invite"?: string}` and refuses unknown fields with `422`. It answers
`400 invite_invalid`, without running the helper, for a string that could never be an
invite; `403` with the coordinator's reason (`invite_required`, `invite_invalid`,
`invite_expired`, `invite_used`, and for a provisioned Pond
`device_certificate_invalid`, `device_revoked`, `device_used`);
`409 no_pending_registration` when the node has no pending registration;
`503 registration_unavailable` for any other registration failure; and
`503 enrollment_unavailable` when enrolling the Pond's own node fails.

### Disabling and signing out

Disabling remote access stops local networking and preserves the pairing. Logout
waits for acknowledged Pond revocation before clearing credentials, and also
removes the device from the registry so it does not linger as one that is merely
offline. Removing the phone in the dashboard does the same. The Pond queues
network revocation durably and retries while coordination is unavailable; application
session and refresh credentials are revoked together. Corrupt identity files cause
visible failure. Restore the private identity backup rather than deleting it to
create an unrelated household.

### The enrollment record and the revocation queue (2026-10-12)

`embedded-network/enrolled.json` lists the phones this Pond enrolled, so a removal
revokes a listed phone without asking the coordinator first, and skips a device a
complete record does not list (a revocation of a device that never enrolled leaves
a tombstone on the coordinator). `revocations.json` is the durable queue the
30-second retry loop sends.

- **A phone is recorded before the helper is asked to enroll it**, in
  `register_phone` and in an approved recovery. The enrollment call can take up to
  25 seconds; a removal, a logout or an absence sweep in that window used to find
  no record, queue nothing, and leave the enrollment the coordinator then
  completed with nothing to revoke it, and so did a crash before the record was
  written. Now the removal queues an unchecked revocation and the phone is answered
  `409`. A coordinator refusal (helper exit 3) is definite, so a device recorded
  for that call alone is forgotten again, unless the coordinator's `inspect`
  reported an enrollment of its own for the device. A timeout or an unreachable
  coordinator (exit 1) keeps the record: an unchecked revocation of an enrollment
  that never happened is the safe way to be wrong.
- **An unreadable `enrolled.json`** (corrupt, or left `0644` by a restore) is moved
  aside as `enrolled.json.corrupt-<unix time>`, logged as
  `kind="enrollment_record_replaced"`, and read as an incomplete record, so
  removals check with the coordinator before revoking. It used to make `DELETE
  /devices`, logout and enrollment answer `503` until someone repaired the file.
  `revocations.json` is still refused when unreadable: dropping queued revocations
  would be the unsafe failure.
- **No household authority means nothing can be checked.** With an enrollment origin
  configured but no `embedded-network/authority/identity.json`, the helper exits 1
  for every coordinator call. A removal of a phone the record lists is still queued:
  its entry backs off until the authority is restored from its backup, and then goes
  out. A removal that would need a check with the coordinator first queues nothing,
  and such an entry already queued is dropped, since nothing can check it. Both are
  logged as `kind="remote_revocation_without_authority"`.

**Drain the revocation queue before downgrading.** Since 2026-09-30 (#434)
`revocations.json` holds an entry per device with its own backoff, which a binary
from before then cannot parse, so it would fail every removal and every retry. An
empty queue is written as `[]`, the form older binaries read, so once
`<data_dir>/embedded-network/revocations.json` reads `[]` (or does not exist) the
downgrade is safe. Keep the coordinator reachable until it drains.

### A phone's superseded node identities (2026-10-05)

A phone runs each pairing's node in its own directory, named after the pairing's
profile: Android `noBackupFilesDir/pond-network/<profile>`, iOS Application
Support `pond-network/<profile>`. Each holds that node's private key and machine
key in `tailscaled.state`. A new pairing gets a new profile, and nothing used to
delete the old directory: on 2026-10-05 the test Galaxy A57 held 33, every one
with keys, from earlier pairings whose identities the app had already discarded.

`mobile.Prune(base, keep)` erases every directory under `base` except `keep`'s.
The companion keeps one pairing at a time, so the native plugins call it on every
`activate`, with the active profile as `keep`, once that profile's node has
started or, with no coordinator, stopped. A forgotten pairing activates with no
profile and keeps none. Local-only keeps its profile, so enabling again reuses the
enrolled node. Prune never touches the running node's directory, and it refuses
any directory whose node lock is held, whatever it believes is running. It writes
the number erased, and every failure, to the event log, and returns the failures
to the plugin, which logs them.

It erases node directories only (2026-10-12, #491). Until then it erased every
entry under `base` whatever its name or type, and removed a symbolic link there.
Now an entry is erased only if its name is a valid profile name, `Lstat` finds a
directory rather than a link or a file, and it holds `node.lock`, `initialized`
or `tailscaled.state`, or nothing at all: an empty directory is what a start that
failed straight after creating it leaves. Anything else, a link included, is
left in place and not followed, and the number left is written to the event log.

### Cached network map on phones (2026-10-05)

A phone's embedded node keeps its last network map on disk and, at a cold start,
runs from it at once instead of waiting for the coordinator. Without it, a phone
that cannot reach the coordinator straight away cannot reach its Pond either, even
when the Pond and the relay are both up. tailscale (v1.102.4, pinned in
`native/pondnet/go.mod`) writes the cache only for a node the coordinator grants
the `cache-network-maps` node attribute. Before this change no node had it, which
is why phones logged `load netmap from cache: netmap cache is not available` at
every start.

**What it stores.** The node's own entry, its peers, the relay map, DNS
configuration, the packet filter and user profiles, one file per item, mode
`0600` in a `0700` directory at `<state>/profile-data/<profile>/netmap-cache/`.
It holds no private keys; those stay in the identity store. The tsnet test in
`native/pondnet/netmapcache_test.go` asserts the file modes and the absence of
private key text.

**Where.** `<state>` is the node's private directory, which is outside every
backup: Android `noBackupFilesDir/pond-network/<profile>`, iOS Application Support
marked `isExcludedFromBackup`. A restored phone starts without a cache and waits
for a live map.

**Who gets it.** Phones only. The enrollment service adds one `nodeAttrs` entry to
the policy it installs. Its targets are the addresses of every active, verified
phone across households, sorted so an unchanged store gives an unchanged entry:

```json
"nodeAttrs": [{"target": ["100.64.0.2", "100.64.0.3"], "attr": ["cache-network-maps"]}]
```

With no such phone the list is empty. A Pond's address is never a target.

**Why not the Pond.** The Pond enforces access: its packet filter decides which
phones may reach it. A Pond starting from a cached map would enforce the rules as
they were when the map was written, so a phone revoked while the Pond was down
could get through until the coordinator answered. The Pond helper (`cmd/pondnet`)
therefore sets `TS_USE_CACHED_NETMAP=false` before it starts its node, so it
neither reads nor writes a cache even if a policy granted it one.

**The stale-peer window.** A phone running from its cache may hold peers and
rules the coordinator has since changed, until its first live map replaces them,
which happens as soon as the coordinator answers. That is acceptable because the
phone decides nothing about access: the Pond always runs from a live map, so a
revoked phone's packets are dropped there, and a removed phone's application
credentials are revoked with it. A cache cannot get a phone anything the Pond does
not currently allow.

**A removed phone.** tailscale does not erase the cache when the coordinator stops
accepting a node. A removed phone still loads it at every start, reports
`Running` on it, and is refused only when the coordinator answers its
registration with a login URL. The phone learns it was forgotten only from its
Pond, which it can reach only on the home network, so without help it would carry
the Pond's addresses and the access rules indefinitely. `mobile.Start` therefore
runs `Node.ForgetNetworkMapWhenRefused` alongside the node: on a login URL,
whether announced on the IPN bus or already in the status when the watch begins,
it clears and removes the cache, keeps the identity, and writes one event-log
line, `the coordinator no longer accepts this device, so its cached network map
was erased`. A node enrolling for the first time has no cache and is left alone.
Measured on the Galaxy A57 on 2026-10-05: after the phone was removed in the
dashboard, a cold start loaded the cache, got `machineAuthorized=false;
authURL=true` from the coordinator, and stayed `disconnected` from the Pond; the
cache stayed on disk until this watcher was added.

**Erasing it.** `mobile.Disable(directory)` stops the node and erases the cache.
It asks the running backend to clear it (`clear-netmap-cache`, which also drops
the backend's in-memory copy), stops the node, then removes the cache directories
from disk. The removal comes last so a map that arrived in between does not
survive, and because the backend's own call does not report a failed delete. A
failure is returned and written to the event log. The identity is kept, so
enabling again needs no new enrollment. `mobile.Stop` still keeps the cache, so a
profile switch or a service stop does not throw it away. The companion app's
Android and iOS plugins call `Disable` when remote access is switched off or the
pairing is forgotten (goose-on-the-go `feature/phone-netmap-cache`, 2026-10-05).

`Disable` erases the running node's cache wherever that node ran (2026-10-12,
#491). Until then it erased only the directory it was given and never compared
it with the running node's, so a caller that passed another profile's directory
stopped the node and left its map on disk. It now records the running node's
directory before stopping it and, when the two differ, erases the cache in both,
writes the mismatch to the event log, and returns every failure joined
(`cacheDirectories`, `native/pondnet/mobile/mobile.go`).

**Kill switch.** `TS_USE_CACHED_NETMAP=false` in a node's environment turns the
cache off. tailscale reads the knob on every check, and with it off it neither
loads nor writes a cache.

**Rollout.** The attribute is part of the coordinator's policy, so it takes effect
only once the enrollment service is redeployed; the service reinstalls its policy
on start and on every enrollment change. Headscale 0.29 accepts `nodeAttrs` with
address targets and passes this attribute through. The live Headscale test
(`enrollment/headscale_live_test.go`) is not part of the default run.

Measured on the pilot coordinator (Headscale 0.29.3) and the Galaxy A57 on
2026-10-05, after the enrollment service was redeployed. `headscale policy get`
showed `"nodeAttrs":[{"target":["100.64.0.2"],"attr":["cache-network-maps"]}]`,
the phone's address and not the Pond's. Four cold starts on mobile data reached
the Pond in 3.6 to 3.7 s, against about 9 s before the cache. The two
launches straight after the reinstall and switching Wi-Fi off took longer than
25 s and 30.6 s; their logs were not kept. One cold start, timed from the app process starting:

| Time | Event |
|---|---|
| 0.32 s | `Start: loaded netmap from disk cache; 1 peers` |
| 0.34 s | `Starting -> Running`, with no wait for control login, which used to take 3.4 s |
| 1.41 s | relay connected |
| 3.05 s | the app reaches the Pond |

The live map replaced the cached one afterwards.

## Verification

Use the security tests and `scripts/live-test.sh` against scratch data, including
a restart with populated databases. Device acceptance additionally requires
physical Android/iOS phones and a Jetson: home LAN, cellular with embedded networking, another Wi-Fi,
and home LAN again. Exercise app/server restarts, LAN address changes, unavailable
coordination/relay service, certificate renewal, and incorrect-pin rejection by both REST and SSE.
Disconnect the separate Tailscale app for these tests. A build or unit-test pass
does not establish physical roaming acceptance. See the current
[embedded verification ledger](embedded-remote-access-verification.md) for measured
simulator, scratch-Pond, backup/restore and remaining hardware results.

This branch includes W3 authorization checks for notification ownership, bearer
revocation and protected diagnostics. HTTPS is independent of those checks. See
[the security posture](auth-network-posture.md) for the implemented contract and
remaining bearer-token risks.

## Backing up the Pond's irreplaceable state

The household authority is an Ed25519 private key at
`<data_dir>/embedded-network/authority/identity.json`. Losing it means a new household:
there is no cloud account recovery, and every paired device must pair again. The HTTPS
identity (`tls/identity.json`; `tls/tailnet-identity.json` beside it is rebuilt from it if
lost) and the WireGuard node state
(`embedded-network/node/tailscaled.state`) must be restored *with* it, because trust is
the combination and not any one of the three.

Enabling remote access creates the household key when `embedded-network/authority/`
does not exist at all (2026-10-05). A Pond restored without that directory therefore
registers a new household when remote access is enabled, instead of failing. A directory
that exists but has lost `identity.json` is still refused, never replaced. Restore the
authority before enabling remote access, and pair every phone again only if it is truly
lost.

`scripts/pond-snapshot.py` streams a tar of exactly that state to standard output:
the three items above, `secrets/`, `secrets.json`, the schedules, and consistent copies
of `pond_system.db` and `pond_vectors.db` taken through SQLite's online backup API so
the Pond keeps serving. It deliberately omits `models/`, `hf_cache/`, `bin/`, `lib/` and
the logs, which are gigabytes and all refetchable; the remainder is under a megabyte.
It also omits `embedded-network/device`: the device key admits a household only on its
first registration, a restored Pond's household is already registered, and leaving it
out keeps a copy of that key off every backup host.

An `embedded-network/authority/` directory with no `identity.json` is refused as a lost
key (`authority identity is missing; restore its backup`). If a crash during first setup
on a build before 2026-09-30 left it that way, and no household was ever registered,
remove the directory and the Pond creates a new authority.

Run it from an operator machine so the Pond needs no additional software, no elevated
privileges and no writable scratch space, and so the archive lands somewhere the Pond's
own disk failure cannot reach:

```
ssh <pond> 'python3 -' < scripts/pond-snapshot.py | age -R <recipients> -o pond-state.tar.age
```

Encrypting on the operator machine to an age recipient keeps the private key off the
Pond, matching the coordinator's arrangement in `deploy/remote-access/`.

One trap when verifying such an archive: the databases are produced by SQLite's backup
API, so they carry a WAL journal-mode header but no `-wal` sidecar. They open normally,
but an explicit read-only open (`file:...?mode=ro`) fails with `unable to open database
file`, because SQLite cannot create the write-ahead index. That is a property of the
verification command, not a corrupt backup; check integrity with an ordinary connection.
