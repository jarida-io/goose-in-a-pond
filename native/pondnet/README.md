# Embedded Pond networking

This pinned Go module supplies the Pond helper, mobile bindings, authenticated
CONNECT proxy, and pilot enrollment service. It uses an explicit Headscale HTTPS
origin and disables the Tailscale diagnostic uploader. It does not install an OS
VPN or change another application's routes.

## Transport boundaries

Mobile native networking obtains an ephemeral loopback proxy endpoint and random
credential directly from `mobile.ProxyConfiguration`. Neither value crosses the
JavaScript bridge. The proxy permits only configured tailnet IP/port pairs. It
forwards TLS bytes without terminating the Pond's HTTPS connection; the native
client still checks the public-key pin, certificate validity, and hostname.

`Node.Dial` uses `internal/tailnetdial`, which calls the userspace TCP stack directly.
Do not replace it with `tsnet.Server.Dial`: the general-purpose tsnet dialer can use
system routes when a destination is absent from its peer map. A separately running
VPN must never become an implicit fallback. The subsystem API is version-sensitive;
every Tailscale update must pass the real Headscale isolation and relay tests.

On the Pond, the helper serves the existing TLS identity and forwards requests over
a private Unix socket. Only this boundary supplies the actual embedded peer address.
Public listeners ignore client-supplied forwarding identity. LAN-only pairing,
session authorization, and rate limiting remain enforced by the Pond API.

## Identity persistence and enrollment recovery

`node.lock` is both the exclusive process lock and the installation marker. Back
up the entire private node directory, including this file and `tailscaled.state`.
An existing installation with missing, empty, malformed, insecure or incomplete
identity state fails before starting networking. Valid earlier state files retain
their keys. The store binds an identity to its configured coordinator, writes
atomically with file and directory synchronization, and becomes unavailable after
a persistence failure until storage is repaired and the process restarted.

The public machine key is available before registration; the public node key is
reported by the coordinator-backed status after registration. Neither exposes a
private key. Enrollment approvals bind the pending authorization ID and machine
key to one locally paired device. If Headscale completes registration but its
response is lost, reconciliation can match the durable intent to exactly one
inventory entry with that machine key and the correct household. It validates
node identity, addresses, permissions and uniqueness before granting access, and
never repeats the registration mutation. Preexisting unmanaged machines cannot be
claimed by this mechanism; a definite coordinator rejection remains failed even
if a matching node later appears. Revocation retains a machine tombstone
so a late registration cannot regain access and is queued for deletion.

The authority helper supports signed `inspect` and `replace` operations. Inspection
returns a durable, non-secret revision for one household/device. Explicit replacement
must name that exact `expectedRevision` and a new pending machine identity. Only
pending, failed or revoked phone records may be replaced; active nodes and the Pond
identity cannot. Each revocation changes the revision, including repeated revocations,
so an older replacement approval cannot reopen access. Replacement never happens as
an automatic enrollment retry.

Retired machine bindings remain in enrollment backups and are never reusable. The
reconciler removes late registrations for those bindings only within their original
household. Each household has a maximum of 256 retired records; hitting the bound
fails visibly without discarding history. Legacy pending records can be explicitly
replaced after inspection, but cannot be recovered automatically without a machine
binding.

The service/helper boundary is tested. The phone-facing local review, cancellation
and credential revalidation workflow is still unfinished, so normal app enrollment
continues to refuse previously enrolled device IDs. Do not erase identity files or
enrollment records to bypass that boundary.

## Building and testing

Use the Go toolchain pinned by `go.mod`. From this directory:

```sh
go test -race ./...
go vet ./...
```

`scripts/build-network-helper.sh` in the Pond repository builds the installed
helper. GOTG's `scripts/build-embedded-network.sh` builds its Android AAR and iOS
XCFramework with the pinned gomobile/gobind tools. Native binaries are generated
artifacts and must not be committed.

`internal/proxyfixture` is a test executable, never a bundled service. Its dialer
maps allowlisted destinations to one explicit IPv4-loopback TLS fixture. GOTG's
Apple and Android proxy suites use it to exercise the production CONNECT proxy.
These client tests do not enroll a mobile node; real Headscale enrollment and
WireGuard isolation are tested separately in `enrollment/headscale_live_test.go`.

See [the verification ledger](../../docs/embedded-remote-access-verification.md)
for measured results and unfinished acceptance work, and
[the deployment guide](../../deploy/remote-access/README.md) for pilot infrastructure.

### Redistribution notices

`THIRD_PARTY_NOTICES.txt` contains the union of the macOS, Linux ARM64, Android
ARM64 and iOS ARM64 dependency notices, plus the Go runtime license. Regenerate
it with `python3 scripts/update-network-notices.py` from the repository root after
changing the module graph. The generator pins go-licenses v2.0.1 and fails on
missing dependency licenses or conflicting platform notices. Its first-party
exclusion still traverses every third-party dependency.

The helper prints the bundled text with `pondnet --third-party-notices`. GOTG's
native build copies this file into the Android assets and iOS pod resources.
