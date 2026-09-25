# Pinned HTTPS implementation and verification

## Source dependencies

Both fork and upstream remotes were fetched on 2026-09-19. Pond was rebased
onto upstream main `d0bccd41`; GOTG onto upstream main `aee8e4d`, retaining its
keyboard adjustment as `e7f374d`. W1 and the former integration dependencies
have landed upstream. Both fork main revisions are ancestors of the selected
bases. Feature work and unrelated local changes were restored after rebasing.
Both repositories remain on `feature/pinned-https-roaming`.

Post-rebase checks: Pond formatting, the full API suite, production compile
check and debug server build pass. GOTG passes 140 JavaScript tests, typecheck,
and lint (zero errors, 19 warnings). Apple transport core tests pass on Xcode 27.
The older results below are pre-rebase unless explicitly stated otherwise.

`npm ci` for the updated Pond desktop fails on the upstream dependency graph:
HeroUI 3.2.5 requires react-aria ^3.52.0 while the lock contains 3.51.0.
An untouched upstream-main package/lock snapshot reproduces this failure.
No peer-dependency checks were disabled. Post-rebase desktop tests are blocked
until that dependency graph is repaired.

Full Xcode 27.0 (27A266a) and the iOS 27 simulator runtime are installed.
On 2026-09-20 the iOS simulator passed all 17 app integration checks using
actual Expo fetch and React Native XHR/SSE against local TLS fixtures and a real
scratch Pond. Correct pins connect; wrong pins, expired certificates, hostname
mismatches, redirects, plaintext and cleared trust are rejected before HTTP.
SecureStore commit/restart persistence, background/foreground resume, real Pond
pairing, authenticated REST, token refresh and live SSE all pass. The scratch
Pond completes onboarding through its normal API, with auth guards enabled.

The iOS build required SDK 56 compatibility fixes for nullable Expo JSI callbacks,
resource-bundle deployment targets, incompatible precompiled Expo modules, and
the scene lifecycle required by iOS 27. They survive prebuild; no trust or linker
validation was weakened. Final production Debug/simulator and unsigned device
Release builds both pass with the runtime fixes. A separate simulator identity
also launches the normal companion home screen, with no production profile.
Android dev and
release builds and nine native TLS tests pass after the rebase.
The post-rebase scratch server test passed both fresh and populated runs,
including unchanged SPKI identity and clean shutdown of both listeners.

No database migration, production signing-key change, or W3 authorization
expansion is included. The transport identity is newly persisted in the Pond
data directory and must be backed up as a secret.

## Earlier local checks (before the rebase unless stated above)

- Pond production `cargo check -p pond-server --all-targets --offline` passes.
- The Pond API suite passes, including the added real-adapter boundary tests:
  remote legacy/init/verify denial, ignored forwarding headers, local-started
  challenge denial remotely, remote refresh, missing-peer refusal, and no
  companion dashboard assets.
- TLS identity tests pass for persistence, address renewal with unchanged pin,
  expired-certificate renewal, corrupt/missing material, mismatched key, unsafe
  permissions, and symlinks.
- The existing `scripts/live-test.sh --no-build` passes on scratch data, including
  populated restart and authentication with the loopback bypass disabled.
- `scripts/test-pinned-https.py` passes for pinned TLS, invalid-pin rejection,
  plaintext rejection, local compatibility, companion asset exclusion, populated
  restart with the same SPKI, and clean shutdown.
- Desktop build and all 833 Vitest tests pass.
- GOTG typecheck and 139 JavaScript tests pass, including both native platforms' trust gating. These include migration, failed
  secure commit preserving the old profile, request trust gating, safe retries,
  coalesced/stale-safe recovery, return to LAN, and shared SSE deduplication.
- Android development/release builds and nine native TLS tests pass. Native
  tests use independent clients from the shared factory, with valid and invalid
  pins, hostname/expiry rejection, key-preserving renewal, redirects, plaintext,
  unrelated traffic, and cleared trust.
- The Apple Foundation/Security transport core passes real TLS checks on macOS:
  correct SPKI, same-key renewal, expiry, malformed DER, separate session clients,
  incremental SSE, hostname mismatch, wrong pins, redirects, plaintext, active
  stream cancellation and unrelated traffic. Rejected requests send no HTTP
  headers. Clean iOS prebuild using the matching SDK 56 template succeeds; this
  does not replace a full Xcode iOS build.

## Existing gates that remain separate

The old GOTG lint failures were present on the earlier integration base. They
are resolved by upstream changes; the post-rebase full lint run has no errors.

Strict Rust Clippy stops in unchanged `pond-voice/src/text.rs` on ten existing
style lints with the installed compiler. That file is identical to the feature
base. CI also reports private Cargo Git dependency access failures and existing
RustSec/OSV advisories. These are not resolved by TLS and no checks were weakened.
Desktop Playwright reports 137 passed, four skipped and nine failed. All nine
failing tests also fail on an archive of the feature base (voice wiring, model
cleanup/status, account fields, session ID display, and the old preview button).
Base main CI run `35083445751` independently fails cloning the private llama-cpp
dependency because GitHub credentials are unavailable.

## Hardware acceptance still required

The isolated Jetson CUDA release build reached the final link and failed on
duplicate GGML symbols from Whisper and llama.cpp. Their dependency declarations
are unchanged by this feature; no duplicate-symbol suppression was added. The
original checkout and its uncommitted changes were preserved, and the production
service was restored with one server process. This failure prevents Jetson live
acceptance. On 2026-09-20, the physical Galaxy A57 (SM-A576B, Android 16) passed
all 17 companion native integration checks in an isolated Release test app.
Pairing, authenticated REST, refresh, and SSE used direct home Wi-Fi to a freshly
compiled W2 Pond on the Mac with scratch data. USB-forwarded certificate fixtures
verified real Expo fetch and React Native XHR/SSE, wrong-pin rejection before HTTP,
expiry, hostname mismatch, redirects, plaintext refusal, cleared trust,
deduplication, foreground resume, and secure persistence after process restart.
The existing companion installation was preserved, temporary listeners and USB
forwarding were removed, and the Jetson production service remained active.
The shared runner is documented in GOTG's `docs/PINNED_HTTPS.md`.

The physical roaming matrix has not been performed. The last Jetson check found
no `tailscale` executable on PATH; the operator confirmed the phone is signed in.
The full LAN → cellular/VPN → other Wi-Fi → LAN matrix and unavailable-VPN
behavior remain hardware acceptance items. Native iOS transport and app-level
simulator verification pass as recorded above.
Physical iPhone roaming acceptance is explicitly deferred by the operator because no iPhone is available. The feature must not be described as fully deployed or completely verified
until those checks are recorded.
