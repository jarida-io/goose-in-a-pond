# Remote-access coordination service

## Scope and trust boundary

This directory deploys the coordination side of remote access: Headscale with its
embedded DERP/STUN relay, the enrollment service (`pond-enrollment`, built from
[native/pondnet](../../native/pondnet/README.md)), and a Caddy gateway. The hosted
instance Ponds use by default answers at `controlpond.jarida.io` (Headscale) and
`enrollpond.jarida.io` (enrollment); a household can run its own from this directory
and name both origins on its dashboard. How the Pond and phones use it is in
[docs/remote-access.md](../../docs/remote-access.md).

Application HTTPS terminates on the Pond. Headscale sees node metadata; the enrollment
service stores household public keys, device/node mappings, permissions and replay
records; DERP forwards WireGuard ciphertext. The operator can neither read household data
nor use a Pond, but can deny or disrupt remote access, which is why self-hosting stays
possible. Nothing depends on Tailscale's hosted control plane, with one exception under
Residual risks.

Every image is pinned by digest, and every container runs as UID 65532 with a read-only
root, no capabilities and `no-new-privileges`. Caddy listens on 8080 and 8443 inside its
container and Docker maps 80 and 443 onto them; it keeps one capability,
`NET_BIND_SERVICE`, only because its image marks the binary with it and the kernel
refuses to exec such a binary without it. Three networks (`compose.yaml`):

| Network | Members | Purpose |
|---|---|---|
| `edge` | gateway, Headscale | public control plane |
| `enroll` (`172.31.250.0/28`, pinned) | gateway, enrollment | lets enrollment trust only its own gateway's `X-Forwarded-For` |
| `admin` (internal) | enrollment, Headscale (as `headscale-admin`) | the admin key never crosses the gateway's network |

Headscale serves its control plane and admin API on one listener, `:8080`, attached to
both `edge` and `admin`, so anything on `edge`, the gateway included, can reach
`/api/v1` there; what the split keeps off `edge` is the admin key, which only enrollment
holds. Publicly, Caddy answers `404` for `/api/*`, `/metrics`, `/debug/*`, `/swagger*`
and `/web*` on the Headscale host, and on the enrollment host proxies only
`/v1/household`, `/v1/approval` and `/health` with an 8 KB body limit. Headscale's
metrics and gRPC listeners are bound to its own loopback. Both sites send HSTS. No API
key belongs in an image, environment file, repository or phone.

## First boot

Use a dedicated host. Copy `headscale.example.yaml` to `runtime/headscale.yaml`,
replacing `server_url` with the selected HTTPS origin and `dns.base_domain` with an
operator-owned namespace. Create private persistent `runtime/headscale`,
`runtime/enrollment`, `runtime/caddy-data`, `runtime/caddy-config` and `runtime/secrets`
directories. Every container runs as UID/GID 65532, so give it the first four (mode
0700) and `runtime/headscale.yaml` (mode 0400); keep `runtime/secrets` root-owned. Set `HEADSCALE_HOST` and `ENROLLMENT_HOST` in a deployment-local
`.env`; these are public names, not credentials.

1. `docker compose up -d headscale`. Leave the gateway off.
2. Create a short-lived admin key with
   `docker compose exec -T headscale headscale apikeys create --expiration 24h`,
   redirecting its output under umask 077 straight to `runtime/secrets/headscale_admin`,
   then give that file to UID/GID 65532 with mode 0400. Never copy the value into
   commands, tickets, logs or shell history. A Compose file secret is a bind mount, so
   the container sees the host file's ownership: left root-only it is unreadable to the
   enrollment user and enrollment never becomes healthy. `docker compose exec -T`
   consumes standard input, which silently truncates the rest of a script piped to
   `ssh <host> bash -s`; redirect it from `/dev/null` in such scripts.
3. `docker compose up -d --build enrollment`. At start it reconciles with Headscale and
   installs the complete default-deny policy. If it cannot, it still reports healthy but
   degraded (Health and logs), so check `docker compose logs enrollment` for
   `enrollment service degraded` before going on.
4. `docker compose up -d gateway`. It waits for healthy enrollment. Check HTTPS health
   and that `/api/v1/user` on the public Headscale host returns 404.
5. Rotate the admin key before it expires with `sudo ./rotate-admin-key.sh [90d]`
   (Key rotation, below).

Port bindings default to loopback. Publishing is an explicit step: set
`HTTPS_BIND=0.0.0.0` and `STUN_BIND=0.0.0.0` after the host firewall and DNS are
configured. The Docker networks are not an authorization boundary against host
operators; only trusted operators may manage this host.

State written by root (an older layout) must be handed to the unprivileged user once,
with the stack stopped:

```sh
docker compose down
sudo chown -R 65532:65532 runtime/headscale runtime/caddy-data runtime/caddy-config
sudo chown 65532:65532 runtime/headscale.yaml && sudo chmod 400 runtime/headscale.yaml
install -m 600 backup.env.example runtime/secrets/backup.env   # then set BACKUP_OFFSITE
docker compose up -d
```

Then confirm the gateway renews certificates, `/health` answers on the enrollment host,
a paired phone still connects, and `sudo ./rotate-admin-key.sh` and `sudo ./backup.sh`
complete.

## How a household joins

A Pond registers its household itself: it sends its public key, companion port and
either an invite or a device certificate, signed with that key, to `/v1/household`. The
service names the household by digesting the key rather than trusting what was sent,
spends the invite or certificate, creates the household's Headscale user and stores it,
in one write. Repeating the call returns the same household and needs no second invite,
so a lost response costs nothing. The service holds at most 1000 households, and
registration is rate limited per source address.

The service decodes registrations strictly and answers `400` to fields it does not
know, so deploy a service version before Ponds that send something new.

### Invites

Issue one on the host running the service:

```sh
docker compose exec enrollment /pond-enrollment --state /state --issue-invite --expires 72h
```

It prints `giap-inv1-XXXX-XXXX-...` once; only its digest is stored. It lasts seven days
by default and at most 30 (`--expires`, up to 720h), admits one household, and can be
typed in any case with or without dashes. The household pastes it into Remote access on
its Pond. Refusals are a closed set: `invite_required`, `invite_invalid`,
`invite_expired`, `invite_used`.

Withdraw an unspent invite with:

```sh
docker compose exec enrollment /pond-enrollment --state /state --revoke-invite giap-inv1-XXXX-...
```

Admin commands talk to the running service over `admin.sock` (mode `0600`) in the state
directory; `--admin-socket` moves it. Without `--state` or `--admin-socket` they stop and
say so rather than looking in the current directory.

An invite bounds who can consume this service's users and addresses. It does not
separate households: the policy does, granting each phone its own Pond's HTTPS port and
nothing more.

### Device certificates

A Pond the operator imaged needs no invite. Its device key is created on the Pond; the
operator signs the public half on the operator's own machine with an offline
provisioning key; the certificate goes back onto the Pond. Its first registration carries
the certificate and a signature by the device key over the household it registers, so a
proof seen in transit admits no other household. The service checks the certificate
against the provisioning keys it trusts, admits one household per certificate, and
records the admission in the same write that creates the household.

Make the provisioning key once, on the operator's machine, and keep it offline
(`go build -o pond-provision ./cmd/pond-provision` in `native/pondnet`):

```sh
pond-provision keygen --key ~/jarida-provisioning.key
```

It prints the public half (`pond-provision public --key ...` prints it again). Give the
service that, and only that, by adding to the enrollment `command` in `compose.yaml`;
repeat the flag to trust a second key while rotating:

```
--provisioning-key <base64 public key>
```

Without `--provisioning-key` the service ignores certificates and only invites admit.
`bash scripts/giap.sh provision --host HOST --key FILE` (`scripts/provision-device.sh`)
images a Pond over SSH: it saves the Pond's setup card, runs
`pondnet --device-action create` there, signs with `pond-provision sign --key ...
--device-public-key ...` here, and installs with `pondnet --device-action install`, which
prints the certificate's serial. The device key never leaves the Pond; the provisioning
key never leaves the operator's machine.

Certificate refusals are a closed set: `device_certificate_invalid` (any check failed,
deliberately not saying which), `device_revoked`, `device_used`. The certificate is tried
first and an invite is the fallback. Revoke a lost or stolen Pond's certificate by its
serial:

```sh
docker compose exec enrollment /pond-enrollment --state /state --revoke-device <serial>
```

Revocation stops the certificate admitting a household from then on; a household it
already admitted keeps its registration, since it holds its own key. A certificate has no
expiry. If the provisioning key is lost or exposed, make a new one, drop the old
`--provisioning-key` and redeploy; every unspent certificate it signed then stops
admitting.

### Limits

| Limit | Value |
|---|---|
| Enrolled devices across all households (`EnrollmentCapacity`) | 4096, then `503 capacity` |
| Coordinator inventory read (`MaxNodes`) | 8192 nodes, streamed node by node |
| Request body | 8 KiB |
| Concurrent operations | 8 |
| Public requests | 10/s, burst 20 |
| Coordinator response headers | 10 s |
| One request / startup or reconciliation | 15 s / 60 s (a reconciliation reads the inventory once, twice when it changed something) |

Source addresses come from the gateway's last `X-Forwarded-For` hop, believed only from
`--trusted-proxy` (the pinned `enroll` network); without it every client shares the
gateway's budget. IPv6 clients are grouped by /64. A full source table first forgets
sources with no allowed request for ten minutes or whose budget has refilled; a source
still rate limited is never forgotten, so nobody can reset a victim's budget with enough
addresses. Approvals are limited per source and, after the signature is checked, per
household.

## Provisioning a household by hand

For an operator-created household or a self-hosted service without invites: on the
Pond's Pairing page, under Remote access, open **Use my own coordination service** and
choose **Prepare household identity**. Transfer only the household identifier and public
key it shows. The private Ed25519 identity stays in the Pond's
`embedded-network/authority`, separate from the HTTPS and WireGuard keys.

Create a Headscale user for the household with `headscale users create` and record its
numeric ID. Stop enrollment (its state is exclusively locked) and run the enrollment
image with the same state mount and:

```
--state /state --provision HOUSEHOLD_ID --public-key PUBLIC_KEY --user-id USER_ID --https-port 4443
```

Use the Pond's actual HTTPS port if it differs. Restart enrollment and confirm health,
then enter the two origins on the Pond and enable remote access. After local phone
pairing, the app's **Enable remote access** requests a device-bound registration through
the pinned local Pond; **Keep local only** makes no coordination contact.

## Enrollment, replacement and revocation

Approvals expire after two minutes, are single-use, and bind the household, device,
role, exact Headscale pending authorization ID and public machine key. A signed approval
is spent as soon as its signature checks out, even if it is then refused. The returned
node is validated before policy grants access, and clients cannot supply the
administrative user or ACL. An ambiguous registration stays pending and is never
replayed; reconciliation recovers only an exact, unique match for the approved machine
identity in the correct household, and cannot claim a preexisting unmanaged node or undo
a definite rejection.

A `failed` record keeps its machine key as a tombstone, like a revoked one: a node the
coordinator registered that the service then refused (for its tags, routes or owner) is
deleted at once and again by every reconciliation until it is gone. A returned node under
another machine key matches no tombstone, so the request deletes it itself unless a record
already holds that node or key. Deletion needs the household's own coordinator user; a
node registered to another user is kept and logged. Revocation wins over pending recovery
and keeps the machine identity as a tombstone, including when a registration appears after
the first revocation attempt. Do not erase the enrollment store to retry.

The service exclusively owns the policy and node mappings. Do not reassign users, nodes,
addresses, tags, routes or policies behind it. Cross-household, phone-to-phone, other Pond
ports, subnet routing and exit nodes have no allow rule. The policy also grants the
`cache-network-maps` node attribute to every active, verified phone's address and never a
Pond's; the service reinstalls its policy at start and on every enrollment change.

Before applying policy the service verifies active node keys, IDs, owner, address and the
absence of tags or approved routes against Headscale. It clears retired node addresses
after removal, so a reused address cannot inherit permissions. Revocation never deletes a
mismatched node from a restored or replaced coordinator database. Paired device IDs reach
the service only as domain-separated SHA-256 enrollment IDs.

Revocation depends on Headscale ending the node's session, and before 0.29.4 it did not
always: a deleted node's long poll stayed open, so the client stayed connected
([#3449](https://github.com/juanfont/headscale/pull/3449)), and an expired node stayed
online until it reconnected ([#3472](https://github.com/juanfont/headscale/pull/3472)).
The compose file pins 0.29.6 (2026-10-24), which also carries Go 1.27.3 against the
leaked eMudhra intermediates (CVE-2026-111943) and the `x/net` HTTP/2 fix (GO-2026-6617).
Do not pin below 0.29.4. The 0.29.6 image reports `v0.29.6+dirty`; its commit,
`a2b1d786`, is the `v0.29.6` tag.

### Explicit replacement

`pondnet --authority-action inspect` signs a single-use request for one device and
returns its current `revision`. `--authority-action replace` signs the same
household/device, `role: "phone"`, a new pending `authId` and `machineKey`, an optional
`nodeKey`, and `expectedRevision` from that inspection. The helper adds its own
household, nonce and two-minute expiry; no admin credential is involved.

This is a household authority, not a public recovery endpoint. The Pond replaces an
`active` enrollment only after a fresh local pairing and an explicit approval on its
dashboard, and a `revoked` enrollment only for the bearer's own phone on the home network
(a stood-down record holds no working remote access). A `failed` enrollment needs the
approval too: each replacement retires a record against a bounded budget, and a refused
enrollment can be failed again at will. Active/revoking records, reused machines, stale
revisions, expired approvals and replays fail closed; a newer revocation invalidates an
older replacement approval.

A replacement retires the old record so a late registration under its machine key can
still be deleted. Retired records are bounded at 256 per household and expire: every
reconciliation (each 30 s) compares them with the inventory it read before deleting
anything, stamps `absentSince` on a record the first time no node under the household's
user holds its identity, clears the stamp when one does, and drops the record once reads
at least 24 hours apart have all found it absent. An expired key is no longer refused as
`identity_retired`, deliberately: no node holds it, and enrolling it still needs a fresh
approval signed by the household's own Pond. A key a node does hold is refused as
`identity_already_registered`. A household that replaces 256 phones within about a day is
refused with `409 retired_capacity` until records expire; the Pond answers its phone
`503 {"error":"retired_capacity"}`.

Rolling back to a `pond-enrollment` that does not know `absentSince` makes it refuse the
state file and restart in a loop. Back up `/state` before upgrading; a rollback needs that
backup, or `absentSince` removed from the state file.

The Pond's helper ignores answer fields it does not know. Helpers built before
2026-10-12 refuse them, so add a field to the approval answer only once the Ponds this
service admits run newer helpers. The status set is closed either way: a new status
needs every Pond updated first.

## Backups and restoration

Back up each Pond's private data with `scripts/pond-snapshot.py`
([docs/remote-access.md](../../docs/remote-access.md), "Backing up the Pond"). Losing
the household authority means its backup or a new household; there is no cloud recovery.

For this host, `sudo ./backup.sh` (root, to read volumes owned by 65532) does the whole
sequence: it stops gateway, enrollment and Headscale in that order, checks Headscale's
SQLite integrity and that enrollment's `state.json` (written by atomic rename under a
`store.lock` flock) parses, encrypts one archive with `age` to the recipients in
`runtime/secrets/backup-recipients`, whose private key is deliberately absent from this
host, restarts in reverse order through an EXIT trap, and prunes to `KEEP` (default 7)
archives in `runtime/backups`. It refuses to run without recipients rather than write an
unencrypted archive. Expect about fifteen seconds of downtime; a connected Pond reconnects
afterwards with its machine identity retained. A copy of only one store can restore
inconsistent authorization mappings, which is why both are taken in one stopped window.

Backups must leave this host: `backup.sh` refuses to run until `BACKUP_OFFSITE` says how,
read from the environment or `runtime/secrets/backup.env` (copy `backup.env.example`,
mode 0600), without surrounding whitespace, a trailing carriage return or one pair of
matching quotes.

- `BACKUP_OFFSITE=user@host:/directory`: each archive is pushed with rsync over SSH, then
  a checksumming rsync dry run must find nothing to change before anything local is
  pruned. The far side needs only rsync.
- `BACKUP_OFFSITE=pull`: another machine collects `runtime/backups` and verifies what it
  collected; this host holds no credential to anywhere.

The reference units in `systemd/` (`goose-backup.service`, `goose-backup.timer`, daily
at 03:30 UTC) assume a checkout at `/opt/goose-remote-access`.

### What a pull-mode collector must do

In `pull` mode this host prunes only what it can see was collected:

- After the collector has copied `control-plane-<stamp>.tar.age` and verified its copy,
  it writes an empty `control-plane-<stamp>.tar.age.collected` beside the archive in
  `runtime/backups`, as the account it reads the archives with. It never writes a marker
  for a copy it did not verify.
- `backup.sh` keeps the newest `KEEP` archives regardless and, of the older ones, deletes
  only those with a marker, together with the marker.
- If no marker is younger than `COLLECTED_WITHIN_DAYS` (default 3) while some archive is
  older than that, the run still writes its archive and then exits non-zero, so
  `goose-backup.service` shows failed.

The collector lives on the operator's machine, outside this repository. Add this after
its pull, with `HOST` and `REMOTE` set to the SSH destination and this host's
`runtime/backups` path, and `LOCAL` to the directory it pulled into. The check compares
modification times and permissions as well as content, so pull with `rsync -a`; a copy
taken with `scp` or a plain `rsync` is never marked.

```sh
for copy in "$LOCAL"/control-plane-*.tar.age; do
  name="$(basename "$copy")"
  # Verified: a checksumming dry run of this host's archive against the copy changes
  # nothing, and the rsync itself succeeded (an archive pruned meanwhile fails it).
  if changes="$(rsync -an --checksum --itemize-changes "$HOST:$REMOTE/$name" "$LOCAL/")" \
     && [ -z "$changes" ]; then
    ssh "$HOST" touch -- "$REMOTE/$name.collected"
  else
    echo "not marking $name: the copy is not verified" >&2
  fi
done
```

Where the collector also holds the age identity, decrypting each copy to `tar -t` before
marking it is the stronger check.

### Restoring

Restore into fresh directories with the original ownership and permissions, using the
pinned image versions, and recreate the containers with those directories mounted. Do not
replace files beneath a reused Docker Desktop bind mount: that produced stale-file startup
failures despite valid SQLite data. Verify Headscale's SQLite integrity and that
`state.json` parses before startup. Start Headscale and enrollment with the gateway off;
verify policy installation, two-household isolation, replay rejection and a revoked
device; only then permit ingress. Replace provisioning credentials after a restore.

## Key rotation

`sudo ./rotate-admin-key.sh [expiry]` (default `90d`, gateway running) mints a key, writes
only its key line into the secret (failing when there is none), recreates enrollment on
it, and then expires exactly the key enrollment was using, named by its own prefix, once
Headscale lists the new one and enrollment is healthy on it. Docker's health check passes
on any `200`, and a service whose key the coordinator refuses still answers `200` with
`"degraded":true`, so the script reads enrollment's own `/health` body for up to a minute,
from inside the gateway with its busybox `wget`, and treats `"degraded":true` as failure.
The key is read only at startup, so the order matters: expiring the old key first would
lock the service out. Until that check passes, every failure, an interruption included,
restores the previous key, recreates enrollment on it and expires the new one.

## Health and logs

Enrollment exposes `/health`. A coordinator it cannot reconcile, at startup or on any
later 30-second reconciliation or policy update, leaves it serving but degraded:
`/health` still answers `200` with `"degraded":true`, so the gateway keeps serving, until a
reconciliation succeeds. Causes include an outage, an admin key the coordinator refuses,
and an inventory past `MaxNodes`. The transition is logged once with its cause, and each
deferred reconciliation logs its error. Logs name transitions and failures, never
signatures, auth IDs, QR payloads or credentials. Container logs rotate at 10 MiB, three
files. Alerts, off-host metrics, disk thresholds and credential-expiry notifications are
not configured by this package.

The policy is written only when it changed. Every reconciliation and enrollment change
computes it from the store and the inventory, reads the one Headscale holds
(`GET /api/v1/policy`), and writes (`PUT`) only when the two differ once decoded, so
whitespace and key order never count. An idle cycle is one inventory read and one policy
read: a write makes Headscale re-evaluate the policy for every node and can push a new
network map to every connected phone. A write logs why: `kind=policy_store_changed` (INFO,
expected); `kind=policy_drift` (WARN), when Headscale holds something other than what this
service last wrote, with rule, node-attribute and field counts only, and the policy is put
back, since nothing else should write it; `kind=policy_unverified` (INFO) for the first
difference after a start or a failed write; and `kind=policy_read_failed` (WARN), when the
read fails and the policy is written anyway, enforcing the allowlist rather than trusting
an unknown state.

To check the embedded DERP relay, send STUN a well-formed request. The relay is
Tailscale's STUN server, which drops binding requests carrying neither a SOFTWARE
attribute nor a FINGERPRINT, so a minimal twenty-byte request times out against a healthy
relay, including from the host itself, and reads exactly like a firewall fault. Confirm
the listener with `ss -lunp` inside the container's network namespace, and look for
`derp.server` in the Headscale log at `debug`: the packaged `warn` level hides the line
announcing the STUN listener.

Measured results and open acceptance items are in
[the verification ledger](../../docs/embedded-remote-access-verification.md). Do not
infer physical-phone roaming or deployment readiness from unit tests or a successful
container build.

## Residual risks

**A Pond or phone may ask Tailscale's servers to resolve the coordinator's name.** When
the system resolver cannot resolve the Headscale host, tsnet's control client falls back
to the bootstrap-DNS endpoint of DERP servers from a map compiled into the Tailscale
module (`net/dnsfallback`, wired as `LookupIPFallback` in `controlclient.NewDirect` and
`controlhttp.(*Dialer).resolver` of v1.104.1). Tailscale's default regions are always in
that map, whatever the coordinator's DERP map says, and no setting or build tag turns the
fallback off. Such a request reveals the client's address and the coordinator's host
name; it carries no household data and grants Tailscale nothing, and it happens only when
normal DNS has failed. A request for an upstream option is drafted in
[docs/upstream/tailscale-dnsfallback.md](../../docs/upstream/tailscale-dnsfallback.md).
