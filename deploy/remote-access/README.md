# Goose remote-access pilot deployment

## Scope and trust boundary

This package prepares Goose-operated Headscale, a signed enrollment service, Caddy,
and Headscale's embedded DERP/STUN relay. It is not a public deployment. The public
domains, hosting, backup destination, monitoring ownership, and incident procedures
must be selected before enabling public ingress or claiming cellular acceptance.

Application HTTPS terminates on the Pond. Headscale receives node metadata and the
enrollment service stores household public keys, device/node mappings, permissions,
and replay records. DERP forwards WireGuard ciphertext. This does not complete the
separate application authorization milestone.

Every image is pinned by digest. The enrollment image runs without a shell, as a
non-root user. Headscale's HTTP administration API stays on the private Compose
network; Caddy blocks its public paths. No API key belongs in an image, environment
file, repository, or phone. There are no hosted Tailscale DERP or control defaults.

## First boot

Use a dedicated host. Copy `headscale.example.yaml` to
`runtime/headscale.yaml`, replacing `server_url` with the selected HTTPS origin and
`dns.base_domain` with an operator-owned namespace. Create private persistent
`runtime/headscale`, `runtime/enrollment`, and `runtime/secrets` directories. Give
the enrollment directory to UID/GID 65532 with mode 0700; restrict the others to
root. Set `HEADSCALE_HOST` and `ENROLLMENT_HOST` in a deployment-local `.env` file.
These names are public configuration, not credentials.

1. Run `docker compose up -d headscale` from this directory. Leave the gateway off.
2. Create a short-lived administration key using
   `docker compose exec -T headscale headscale apikeys create --expiration 24h`.
   Redirect its output directly to `runtime/secrets/headscale_admin` under umask
   077, then give that file to UID/GID 65532 with mode 0400. Do not copy the value
   into commands, tickets, logs, or shell history. A Compose file secret is a bind
   mount rather than a copy, so the container reads the host file's own ownership:
   left root-only it is unreadable to the non-root enrollment user, and enrollment
   never becomes healthy. Note also that `docker compose exec -T` consumes standard
   input, which silently truncates the remainder of a script piped to
   `ssh <host> bash -s`; redirect it from `/dev/null` inside such scripts.
3. Build and start enrollment with `docker compose up -d --build enrollment`.
   It installs the complete default-deny policy before becoming healthy.
4. Start the gateway with `docker compose up -d gateway`. It requires healthy
   enrollment at startup. Check HTTPS health and confirm that `/api/v1/user` on
   the public Headscale hostname returns 404.
5. Rotate the administration key before expiry with `./rotate-admin-key.sh [90d]`.
   The key is read once at startup, so the order is load-bearing: mint, install,
   recreate enrollment, confirm it reports healthy, and only then expire the
   previous key. Reversing the last two locks the service out of the coordinator.
   The script restores the previous key if enrollment does not come back.

The Compose port bindings default to loopback. Setting `HTTPS_BIND=0.0.0.0` and
`STUN_BIND=0.0.0.0` is an explicit public-deployment step and is outside this
preparation milestone. Configure host firewall and DNS before doing so. The
control Docker network is not an authorization boundary against host operators;
only trusted operators may manage this host.

## How a household joins

A household registers itself. The Pond sends its public key and companion port,
signed with that key, to `/v1/household`; the service names the household by
digesting the key rather than trusting what was sent, creates its coordinator
user, and stores it. Repeating the call answers with the same household, so a
lost response costs nothing and an operator-created household answers
identically.

Admission proves possession of a key and nothing else, because admission is not
what separates households: the policy is, and it grants each phone its own Pond's
HTTPS port and nothing more. Registration is rate limited per source address, the
source table is capped, and the number of households is capped, because
`--provision` used to be the only thing bounding what admission could consume.

The manual path below remains for an operator-created household and for anyone
running their own coordination service.

## Provisioning a household by hand

On the Pond's local Pairing page, choose **Prepare household identity**. Transfer
only its household identifier and public key to the pilot operator. The private
Ed25519 identity stays under the Pond's `embedded-network/authority` directory,
separate from HTTPS and WireGuard keys.

Create a dedicated Headscale user for the household with `headscale users create`.
Record its numeric ID. Stop enrollment while provisioning because its state is
exclusively locked. Run the enrollment image with the same state mount and:

```
--state /state --provision HOUSEHOLD_ID --public-key PUBLIC_KEY --user-id USER_ID --https-port 4443
```

Use the Pond's actual HTTPS port if it differs from 4443. Restart enrollment and
confirm health. On the local Pond, enter the two Goose HTTPS origins and approve
activation. The Pond signs its pending Headscale registration. After local phone
pairing, GOTG's **Enable remote access** requests a device-bound registration via
the pinned local Pond. **Keep local only** makes no coordination contact.

Approvals expire, are single-use, and bind the household, device, role, exact
Headscale pending authorization ID and public machine key. The machine key is
available before authorization; the returned node is validated before policy grants
access. Clients cannot supply the administrative user or ACL. An ambiguous
registration remains pending and is never replayed. Reconciliation recovers only
an exact, unique match for the approved machine identity in the correct household.
It cannot claim a preexisting unmanaged node or undo a definite registration
rejection. Those failed transitions require explicit local recovery.
Revocation wins over pending recovery and retains the machine identity as a
tombstone, including when registration appears after the first revocation attempt.
Do not erase the enrollment database to retry.

The enrollment service exclusively owns the policy and registered node mappings.
Do not manually reassign users, nodes, addresses, tags, routes, or policies behind
it. Cross-household traffic, phone-to-phone traffic, other Pond ports, subnet
routing, and exit nodes have no allow rule.

## Backups and restoration

Back up the Pond's complete private data directory using its existing backup
procedure. Restoring the household authority, HTTPS identity, and WireGuard state
together preserves trust. Loss of the authority requires its backup or a new
household; there is no cloud account recovery.

For infrastructure backup, stop gateway and enrollment first, then Headscale. Take
a coordinated, encrypted backup of both stores and their accompanying files,
Headscale Noise/DERP identities, configuration, and Caddy state. The two stores are
not alike: Headscale keeps SQLite, while enrollment keeps `state.json`, written by
atomic rename under a `store.lock` flock. Copy the complete Headscale directory,
including any SQLite WAL/SHM files. An independent copy of only enrollment or only
Headscale can restore inconsistent authorization mappings. Keep the backup key
outside this host; never store plaintext archives in Git.

`./backup.sh` performs exactly this sequence and is driven by the reference units in
`systemd/`, whose paths assume a deployment at `/opt/goose-remote-access`: it stops gateway, enrollment and Headscale in order, checks
both stores while nothing is writing, encrypts a single archive to an age recipient
whose private key is deliberately absent from this host, restarts in reverse order
through an EXIT trap, and prunes to `KEEP` archives. It refuses to run at all rather
than write an unencrypted archive. Expect roughly fifteen seconds of downtime per
run; a connected Pond reconnects afterwards with its machine identity retained.

Restore into fresh isolated directories with the original permissions, using the
pinned versions, and recreate the containers with those directories mounted. Do
not replace files beneath a reused Docker Desktop bind mount: the local mobile
restore fixture reproduced stale-file failures there despite valid SQLite data.
Verify Headscale's SQLite integrity, and that the enrollment `state.json` parses,
before startup. Start Headscale and enrollment with the gateway still off. Verify policy
installation, two-household isolation, replay rejection, and a revoked device.
Only then permit ingress. Provisioning credentials should be replaced after a
restore. Unit tests cover authority persistence and corrupt files; the complete
infrastructure backup/restore drill passed locally for a populated household/user
mapping. The Android emulator and existing iOS simulator also pass active mobile
reconnection after full infrastructure restoration: retained machine identity,
authenticated REST, and incremental SSE work with the restored state. These local
fixtures do not establish public cellular availability.

## Health, logs, and remaining verification

Enrollment exposes `/health`, limits request bodies to 8 KiB, concurrent operations
to eight, and public requests to 10/s with a burst of 20. Logs name transitions and
failures, never signatures, auth IDs, QR payloads, or credentials. Container logs
rotate at 10 MiB with three files. Alerts, off-host metrics, disk thresholds, and
credential-expiry notifications must be configured for public operation.

When checking the embedded DERP relay, verify STUN with a well-formed request. The
relay is Tailscale's STUN server, which deliberately drops binding requests carrying
neither a SOFTWARE attribute nor a FINGERPRINT so it cannot be used as a general
reflector. A minimal twenty-byte binding request therefore times out against a
perfectly healthy relay, including from the host itself and against the container
address, which reads exactly like a firewall or publishing fault. Confirm the
listener with `ss -lunp` inside the container's network namespace before suspecting
the network, and confirm `derp.server` in the Headscale log at `debug` level: the
packaged `warn` level suppresses the line announcing the STUN listener.

See `../../docs/embedded-remote-access-verification.md` for measured local results
and remaining gaps. Do not infer physical-phone roaming or complete deployment
readiness from unit tests or a successful container build.


### Revocation and coordinator drift

The Pond durably queues authenticated phone revocation before acknowledging logout.
Retries continue after restart and while embedded networking is disabled. Local-only
households with no enrollment configuration make no coordinator request. Paired
device IDs map to domain-separated SHA-256 enrollment IDs, preserving compatibility
with legacy device identifiers without exposing them to the coordinator.

The enrollment service retains a revocation tombstone to reject delayed enrollment.
It clears retired node addresses after successful removal so a subsequently reused
address cannot inherit permissions or prevent a legitimate new registration. Before
applying policy, it verifies active node keys, IDs, owner, address and absence of tags
or approved routes against Headscale. Revocation never deletes a mismatched node
from a restored or replaced coordinator database.

### Explicit replacement protocol

The private Pond authority helper now accepts `--authority-action inspect` and
`--authority-action replace` in addition to initial enrollment and revocation.
Inspection signs a single-use request for one device and returns its current
`revision`. Replacement signs the same household/device, `role: "phone"`, a new
pending `authId` and `machineKey`, optional `nodeKey`, and `expectedRevision` from
that inspection. The helper supplies its own household, nonce and two-minute expiry;
no administration credential is involved.

This is an operator authority, not a public recovery endpoint. Replacement must
follow fresh local pairing and explicit local review of the exact device and pending
registration. A caller must never automatically inspect and replace after an ordinary
enrollment failure. Active/revoking records, reused machines, stale revisions,
expired approvals and replays fail closed. A newer revocation invalidates an older
replacement approval. Retired identities remain in backups for late-registration
cleanup, with a bound of 256 per household and no automatic deletion.

Current pilot limitations: the service protocol and helper are implemented and tested
against real Headscale, but the phone-to-local-dashboard recovery flow is unfinished.
It still needs credential revalidation at approval and cancellation/revocation race
checks. Legacy pending records without machine bindings cannot recover automatically.
Do not deploy publicly while those lifecycle acceptance items are open.
