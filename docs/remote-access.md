# Remote access with pinned HTTPS

## Connection model

The Android and iOS companion pairs on the Pond's directly attached LAN. Once paired,
it uses HTTPS on the LAN at home and HTTPS inside Tailscale/WireGuard away from
home. HTTPS is HTTP over TLS; WireGuard is an additional encrypted transport.
TLS terminates on the Pond. No cloud service holds its private key.

Run Tailscale clients directly on both the Pond and phone. For Headscale, join
with `tailscale up --login-server=https://headscale.example.com`. No embedded
VPN client is included. Source-masking proxies and subnet routers are unsupported
for pairing: the server classifies the actual TCP peer, never forwarding headers.
Do not expose the companion listener to the public internet. Limit tailnet access
with the coordinator's ACLs. Android permits one active VPN at a time.

## Listeners and local tools

- HTTP binds exclusively to `127.0.0.1`, normally port 4000. Desktop assets,
  development pages, CLI, and OAuth callback integration remain local.
- HTTPS binds to all IPv4 interfaces, normally port 4443, and serves the API.
  `serve --https-port PORT` overrides its starting port. Each listener tries ten
  consecutive ports. Neither listener falls back to plaintext network access.
- Read `<data_dir>/.runtime_api_port` and `.runtime_https_port` for actual ports.
  `/api/v1/system/info` publishes `https_port` and `tls_spki_sha256`.
- mDNS `_pond._tcp.local.` publishes the HTTPS port and `scheme=https`.
  Discovery supplies candidates; it never supplies trusted keys.

To view the dashboard from another computer, use an authenticated SSH tunnel to
the loopback HTTP port. Preserve the existing OAuth redirect port. Such tunnels
are administrative access, not a supported remote mobile pairing mechanism.

## Pairing and identity

Open the local dashboard or run the `pairing` CLI command. The dashboard, CLI,
and startup output use the same payload contract:

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
for tailnet or unclassifiable peers. Knowing a code or starting a challenge at
home does not permit completing it remotely. Code issuance remains loopback-only.
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

## Roaming and failures

A shared manager selects endpoints for REST and foreground SSE. On Wi-Fi it
prefers pinned local addresses, uses mDNS to find a changed address, then tries
the saved tailnet address. Cellular uses tailnet. Network changes and foreground
resume re-evaluate the choice; background probing pauses. Recovery is coalesced,
uses a capped backoff, and stops after six failed attempts until another trigger
or a manual retry. NetInfo does not perform external reachability probes or
collect SSIDs.
On Wi-Fi, six bounded local rechecks also allow a connected phone to return from
tailnet after a temporary LAN outage without waiting for a network-change event.

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
before initialization. Only Pond requests use this protocol; each uses an
ephemeral session with SPKI, validity, and hostname validation. Changing trust
cancels active requests, including SSE. Unrelated traffic uses normal platform
trust. Expo prebuild recreates the source files, bridge, and Xcode integration.
The Apple transport core and 17 app-level iOS simulator checks pass, including
real scratch-Pond pairing, refresh, REST/SSE, invalid-pin rejection, persistence
and foreground resume. Physical roaming remains separate, as recorded in
[verification](pinned-https-verification.md).

## Verification

Use the security tests and `scripts/live-test.sh` against scratch data, including
a restart with populated databases. Device acceptance additionally requires
physical Android/iOS phones and a Jetson: home LAN, cellular with VPN, another Wi-Fi,
and home LAN again. Exercise app/server restarts, LAN address changes, unavailable
VPN, certificate renewal, and incorrect-pin rejection by both REST and SSE.
A build or unit-test pass does not establish physical roaming acceptance.

HTTPS does not complete authorization hardening. See
[the security posture](auth-network-posture.md) for the remaining W3 work.
