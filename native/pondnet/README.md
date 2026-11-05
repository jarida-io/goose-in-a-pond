# Embedded Pond networking

This Go module (`tailscale.com` v1.104.2, Go 1.27.3, both pinned in `go.mod`) supplies
the Pond's network helper, the mobile bindings goose-on-the-go links, the authenticated
CONNECT proxy, and the enrollment service the coordinator runs. It talks to an explicit
Headscale HTTPS origin and disables Tailscale's diagnostic uploader. It installs no OS
VPN and changes no other application's routes.

| Command | Runs on | Purpose |
|---|---|---|
| `cmd/pondnet` | the Pond, beside `pond-server` | the tsnet node and tailnet bridge (`--state --control --socket --identity --port --hostname`), household authority operations (`--authority-action identity\|register\|inspect\|enroll\|replace\|revoke --state --enrollment --port`), device certificates (`--device-action create\|install --state`), `--third-party-notices` |
| `cmd/pond-enrollment` | the coordinator host | the enrollment service and its admin commands; see [the deployment guide](../../deploy/remote-access/README.md) |
| `cmd/pond-provision` | the operator's machine | `keygen\|public\|sign --key FILE [--device-public-key KEY]` for device certificates |
| `mobile` | phones, via gomobile | `Start`, `Stop`, `Disable`, `Status`, `ProxyConfiguration`, `SetTargets`, `SetResolvers`, `SetInterfaceProvider`, `NetworkChanged`, `Prune`, and diagnostics sinks |

`pondnet --authority-action` reads its request as JSON on stdin (8 KiB at most, unknown
fields refused) and exits 0 with the result on stdout, 3 when the coordinator refused
(printing `{"refused": "<identifier>"}`), 1 for any other failure, and 2 for bad input.
`register` reads `{"invite": "..."}`; an empty or absent invite sends none. The invite is
never an argument, since other accounts can read those.

## Transport boundaries

Mobile native code gets an ephemeral loopback proxy endpoint and a random credential
from `mobile.ProxyConfiguration`; neither crosses the JavaScript bridge. The proxy
permits only the tailnet IP/port pairs set with `mobile.SetTargets`, which refuses the
node's own tailnet addresses. It forwards TLS bytes without terminating the Pond's
HTTPS, so the native client still checks the pin, validity and hostname. It holds at
most 64 sockets (`MaxConnections`), closes idle keep-alive connections after 30 s and a
connection after a 407, and refuses Tailscale's service addresses (`100.100.100.100`,
`fd7a:115c:a1e0::53`).

On the Pond, `ServePond` listens on the tailnet at the companion port with the Pond's
tailnet certificate (re-read on every handshake) and `BridgeHandler` forwards to the
Pond's private Unix socket. It forwards only `/api/v1/` paths in origin form, caps its
listener at 64 sockets, closes idle connections after 60 s, and answers
`502 {"error":"embedded_bridge_unavailable"}` when the Pond sends no response headers
within 30 s. Only this boundary supplies the real embedded peer address; public
listeners ignore client-supplied forwarding identity, and LAN-only pairing, session
authorization and rate limiting stay with the Pond API.

`coordinatorClient` ignores proxy environment variables and follows no redirects.
`Submit` refuses a coordinator answer over 4 KiB, with trailing data, an unknown status
or role, or a malformed revision; for an enroll or replace, one naming a different role
or machine key than asked; and for a revoke, one that does not confirm `revoked`
(`decodeAnswer`, `answered` in `authority.go`). It ignores fields it does not know, so
the service can add one without breaking deployed helpers.

`Node.Dial` uses `internal/tailnetdial`, which calls the userspace TCP stack directly.
Do not replace it with `tsnet.Server.Dial`: the general-purpose dialer can use system
routes when a destination is absent from the peer map, and a separately running VPN must
never become an implicit fallback. `internal/tailnetdial/dial_test.go` pins this offline
on every `go test`: two nodes on an in-process control server, a host listener the
dialer must never reach, a canary showing `tsnet.Server.Dial` does reach it, and a peer
round trip. If the canary fails after a Tailscale update, the fallback is gone upstream
and the package can be re-evaluated; every update must still pass the real Headscale
isolation and relay tests.

`cmd/pondnet` sets `TS_USE_CACHED_NETMAP=false` before starting the Pond's node, so the
Pond always enforces access from the coordinator's live map. Phones may cache theirs
(`netmapcache.go`, `Node.ForgetNetworkMapWhenRefused`); see
[remote access](../../docs/remote-access.md).

## Identity persistence and enrollment recovery

`node.lock` is the exclusive process lock. `initialized` is written after the first
identity. Back up the whole node directory, including `initialized` and
`tailscaled.state`; `node.lock` need not be kept. A profile without `initialized` is
new, so a missing identity is created; a profile with it and no identity has lost it and
is refused: restore the backup. An existing installation with missing, empty,
malformed, insecure or incomplete identity state fails before networking starts. The
store binds an identity to its coordinator, writes atomically with file and directory
sync, and refuses further writes after a persistence failure until storage is repaired
and the process restarted. The household authority is built in a staging directory and
renamed into place, so a crash during first setup leaves nothing half-written.

The public machine key is available before registration; the public node key comes from
the coordinator-backed status after it. Neither exposes a private key. The status also
carries `health`, the backend's redacted health warnings (the last control-server error while
it cannot log in, for one), and `cmd/pondnet` adds `clockSynchronized` on Linux; see
[when the node is not running](../../docs/remote-access.md#when-the-node-is-not-running). Enrollment
approvals bind the pending authorization ID and machine key to one locally paired
device. If Headscale completes a registration but its response is lost, reconciliation
matches the durable intent to exactly one inventory entry with that machine key in the
correct household, validates node identity, addresses, permissions and uniqueness, and
never repeats the registration. Preexisting unmanaged machines cannot be claimed this
way, and a definite coordinator rejection stays failed even if a matching node appears
later. Revocation keeps the machine key as a tombstone so a late registration under it
is deleted.

The authority supports signed `inspect` and `replace`. Inspection returns a durable,
non-secret revision for one household/device. Replacement names that exact
`expectedRevision` and a new pending machine identity. Only pending, failed or revoked
phone records may be replaced; active nodes and the Pond's own identity cannot. Every
revocation changes the revision, so an older replacement approval cannot reopen access.
The Pond replaces a record only after local approval of a recovery, or for a `revoked`
record when the bearer's own phone enables remote access from the LAN
(`crates/pond-server/src/embedded_network.rs`, `embedded_network/recovery.rs`); never as
an automatic retry.

A replacement retires the old record, so a late registration under its machine key can
still be deleted within its household. Each household holds at most 256 retired records
(`retiredCapacity`); past that a replacement is refused with `retired_capacity`. A
retired record expires once inventory reads at least 24 hours apart (`retiredGrace`)
have found no node holding its identity, after which that machine key may enroll again
with a fresh approval (`enrollment/retired.go`). Legacy pending records without a
machine binding can be replaced explicitly after inspection, but not recovered
automatically. Do not erase identity files or enrollment records to get around any of
this.

### State on disk

Every file holding a key or state is read through `internal/privatefile`, which opens
with `O_NOFOLLOW|O_NONBLOCK` and checks the open descriptor: a regular file, `0600` or
stricter, within a size bound. Checking the path first and opening after would read a
file swapped in between.

tsnet keeps its own unredacted log ring (`tailscaled.log1.txt`, `tailscaled.log2.txt`)
in the profile whatever the configured logger does. `logtail.Disable()` drops entries
before they reach it, and the helper deletes any such files when a node opens.

## Building and testing

Use the Go toolchain `go.mod` pins. From this directory:

```sh
go test -race ./...
go vet ./...
```

CI (`embedded-network` in `.github/workflows/ci.yml`) also checks `gofmt`, runs
`govulncheck` v1.9.0, builds `./cmd/...` and cross-compiles `cmd/pondnet` for
linux/arm64.

`scripts/build-network-helper.sh [output-dir]` builds the Pond's helper into
`target/release/pondnet` by default, static and trimmed, with mode `0755` (the Pond
refuses a helper anyone but its owner could replace). goose-on-the-go builds its Android
AAR and iOS XCFramework from this module with its own `scripts/build-embedded-network.sh`
and pinned gomobile (unverified here). Native binaries are build artifacts and are not
committed.

`internal/proxyfixture` is a test executable, never a bundled service. Its dialer maps
allowlisted destinations to one IPv4-loopback TLS fixture, and goose-on-the-go's Apple
and Android proxy suites use it to exercise the production CONNECT proxy. Real Headscale
enrollment and WireGuard isolation are tested by `enrollment/headscale_live_test.go`
(`TestHeadscaleLive`) and `identity_live_test.go` (`TestNodeHeadscalePersistence`), which skip unless pointed at a
disposable coordinator; the environment they read is in
[the verification ledger](../../docs/embedded-remote-access-verification.md#reproduction).

### Redistribution notices

`THIRD_PARTY_NOTICES.txt` is the union of the macOS, Linux ARM64, Android ARM64 and iOS
ARM64 dependency notices plus the Go runtime licence. Regenerate it with
`python3 scripts/update-network-notices.py` from the repository root after changing the
module graph. The generator pins go-licenses v2.0.1 and fails on missing dependency
licences or conflicting platform notices. `pondnet --third-party-notices` prints it;
goose-on-the-go copies it into its Android assets and iOS pod resources.
