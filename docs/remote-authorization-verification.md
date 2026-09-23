# Remote authorization verification

## Scope and dependency boundary

W3 is based on `upstream/main` (`d0bccd41`) in `feature/remote-authorization`.
Both remotes were fetched before work. It can be reviewed independently of
[the pinned HTTPS server](https://github.com/jarida-io/goose-in-a-pond/pull/375)
and [Android/iOS roaming](https://github.com/jarida-io/goose-on-the-go/pull/38).
This branch alone still serves HTTP; transport acceptance remains with those PRs.
No database migration, production key change, client identity format change,
or IP binding is included. Existing GOTG requests already attach a bearer to
revocation and use the paired install ID for push registration and SSE.

## Measured checks — 2026-09-20, Mac

- Regression suite on the unchanged base: the own-device roaming control passed;
  anonymous diagnostics, unauthenticated revoke, and cross-device delivery
  checks failed as intended (1 passed, 3 failed).
- Full `SQLX_OFFLINE=true RUSTFLAGS="" cargo test -p pond-api`: **473 passed,
  0 failed, 2 ignored**. This includes the real SQLite-issued sessions, remote
  refresh, revocation scope, another device's notification/push denial, unchanged
  push-token storage after denial, local compatibility and public bootstrap.
- `cargo fmt --all --check`, shell syntax and Python compilation passed.
- Full default-feature `cargo build -p pond-server` passed. The first attempt
  exceeded eSpeak's 180-byte phoneme filename buffer because of the worktree
  path. Using the shorter physical target path and regenerating only that
  dependency's CMake build directory fixed it without source changes.
- `cargo clippy -p pond-api --all-targets --no-deps` passed with 11 warnings;
  every reported source fragment is unchanged in `upstream/main`. The stricter
  `-- -D warnings` diagnostic fails on six pre-existing library warnings
  (argument count, collapsible condition, length comparison, char comparison,
  and type complexity). No lint or CI settings were relaxed.
- `scripts/live-test.sh --no-build` passed against the newly built binary:
  71 first-start checks, 11 populated-data restart checks, the existing no-bypass
  auth/onboarding assertions, and the new W3 real pairing/delivery/refresh/revoke
  checks. All scratch listeners were stopped afterward. The log reported an
  unavailable `nan0` mDNS interface, the deliberate offline-egress refusal, and
  the scratch `mock` model not being in the catalog; none is a W3 regression.

The fixture carries actual `ConnectInfo` extensions through the middleware;
`MockConnectInfo` alone is only an extractor fallback and does not establish the
peer for middleware that reads connection extensions directly.

## Reproduction

```sh
SQLX_OFFLINE=true RUSTFLAGS="" cargo test -p pond-api
SQLX_OFFLINE=true RUSTFLAGS="" cargo clippy -p pond-api --all-targets --no-deps
scripts/live-test.sh
```

For deep worktree paths, set `CARGO_TARGET_DIR` to a short build directory;
the live runner respects that directory when locating the built executable.
The live runner proves the server PID owns its recorded port, starts from scratch
storage, restarts against populated storage, and runs authorization checks with
the loopback bypass off. `scripts/remote_auth_checks.py` performs real two-phase
pairing, verifies delivery ownership, rotates credentials and revokes only the
bearer session. It never prints credentials. Run it through the launcher, not
against production data.

## Remaining acceptance and boundaries

Jetson execution and physical phone roaming remain pending. The iPhone roaming
check is explicitly deferred until a device is available. Simulator acceptance
from the transport PR is not target hardware evidence for this branch.

A stolen bearer still authorizes its recorded device. Already-open SSE sessions
are not rechecked on every event, and concurrent refresh/revoke races need a
separate lifecycle change. A client whose session has expired must refresh it
before authenticated server-side logout; clearing local credentials alone does
not promise that the server revoked a refresh credential. Public bootstrap/onboarding routes and broader
household resource policy are not claimed to be hardened by these delivery guards.

The transport PR's CI currently fails during private Cargo dependency access,
desktop React/HeroUI peer resolution, and Matter lockfile consistency checks;
security advisory gates also remain blocking. W3 does not change or weaken those
checks. Matter package and lock files are identical to the fetched base.
