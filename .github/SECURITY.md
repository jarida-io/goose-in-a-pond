# Security Policy

**Goose In A Pond (GIAP)** is a privacy-first AI smart home assistant that runs
entirely on your own hardware. It listens on your network, holds credentials for
your accounts, can see and hear the room it is in, and can operate physical
devices in your home. A vulnerability here is not an abstract one — we treat
reports accordingly.

---

## Reporting a Vulnerability

**Please do not open a public issue, discussion, or pull request for a security
problem.** Public disclosure before a fix is available puts every running
installation at risk.

Report privately through **[GitHub Security Advisories][advisory]** — use
*Security → Report a vulnerability* on this repository. That channel is private
between you and the maintainers, and it lets us credit you and issue a CVE if
one is warranted.

If you cannot use GitHub Security Advisories, email **security@jarida.io**.

### What to include

The more of this you can provide, the faster we can confirm and fix:

- What the vulnerability lets an attacker do, in one sentence.
- Where the attacker has to be — on the LAN, on the device, an authenticated
  paired client, or a remote party with no prior access. This matters more than
  almost anything else for GIAP (see *Security model* below).
- Steps to reproduce, ideally against a fresh `cargo run -p pond-server -- serve`.
- The commit SHA or branch you tested, plus platform (Jetson, Linux, macOS).
- Any proof-of-concept, log excerpt, or crash dump. **Redact your own secrets** —
  pairing codes, session tokens, OAuth tokens, and API keys are all live
  credentials.

### What to expect

| Stage | Target |
|---|---|
| Acknowledgement that we received the report | 3 working days |
| Initial assessment — confirmed / not reproducible / out of scope, with severity | 10 working days |
| Fix or documented mitigation for Critical and High | 30 days from confirmation |
| Fix or documented mitigation for Moderate and Low | Next regular release cycle |

If we go quiet, please chase us — a missed notification is far more likely than
a decision to ignore you.

### Disclosure

We follow coordinated disclosure. We will agree a disclosure date with you,
credit you in the advisory unless you prefer to stay anonymous, and publish a
GitHub Security Advisory once a fix is available. We ask that you give us **90
days** before public disclosure, and we will tell you promptly if we need longer
and why. We will never take legal action against someone who reports a
vulnerability in good faith under this policy.

---

## Scope

### In scope

- Everything in this repository: the `pond-*` crates, the REST API, the Electron
  desktop app (`pond-desktop`), the GIAP MCP server (`pond-mcp-server`), the
  embedded-networking helper (`native/pondnet`), the Matter controller
  (`matter-server`), the credentials service (`services/pondcredentials`) and the
  deployment kits under `deploy/`.
- Pairing (CPace, the setup code, the spoken code, the BLE setup channel), session
  tokens, and the authorization boundary between an unpaired and a paired client.
- Remote access over the embedded tailnet, and the hosted coordinator and
  credentials service only insofar as this repository's code and deployment kits
  define them.
- Handling of credentials, biometric data, audio, and camera data.
- Build and release integrity for this repository, including CI workflow
  configuration and dependency pinning.

### Out of scope

- **The `goose` submodule.** GIAP builds on [Goose][goose] and vendors it as a
  pinned fork (`jarida-io/Goose`). Report vulnerabilities in Goose itself
  upstream. If a Goose issue is reachable through a GIAP code path, or our pin
  holds a vulnerable version, that *is* in scope for us — tell us and we will
  bump the pin.
- **Third-party MCP extensions**, including anything installed from the
  extension marketplace. Report those to their own maintainers. How GIAP
  *installs, sandboxes and grants access to* extensions is in scope.
- Third-party model weights, and the content an LLM generates. Report a
  *mechanism* that lets model output cross a trust boundary — that is in scope.
- Findings that require an attacker to already have physical access to an
  unlocked device, or root on the host.
- Automated scanner output with no demonstrated impact, missing hardening
  headers with no exploit path, and vulnerabilities in dependencies we do not
  compile into a shipped artifact. See *Known accepted risks*.

---

## Supported Versions

GIAP is **pre-release**. There are no tagged releases yet, and the version in
`Cargo.toml` is `0.1.0` across the workspace.

| Version | Supported |
|---|---|
| `main` | Yes — fixes land here |
| Any other branch, fork, or build | No |

There is no backport channel. Until we cut a first release, "upgrade" means
"pull `main` and rebuild", and `git submodule update --init --recursive` after
any commit that moves the goose pin.

---

## Security model

Knowing what GIAP assumes will tell you quickly whether something is a finding
or intended behaviour.

**The LAN is untrusted.** `pond-server` has two listeners. The dashboard listener
binds `127.0.0.1` and answers only requests addressed to a loopback name from a
first-party origin; pairing codes and remote-access management there also need a
host credential that only the Pond's own OS account can read. The companion
listener binds every IPv4 interface over HTTPS, with a key phones pin at pairing.
Anything that can route to the device can reach the companion listener, so a bug
that lets an unpaired client on the same network read or write data, or reach a
device-control route, is a real finding and we want to hear about it.
`docs/auth-network-posture.md` describes the boundary in full.

**Pairing is the authorization boundary.** Pairing is accepted only from loopback or
a directly attached LAN peer. Phones prove a six-digit code with CPace bound to the
Pond's TLS key; each code allows ten failed confirmations and is single-use. The
desktop pairs over loopback with HMAC-SHA256. A Pond with no screen is claimed with
its printed eleven-digit setup code and pairs later phones with a code it reads
aloud. Session and
refresh tokens are random, and only SHA-256 hashes of codes and tokens are
persisted. Anything that forges, replays, fixates, or brute-forces those exchanges,
or that leaks a token through logs, error messages, or the event log, is in scope.

**Installing an extension is granting code execution.** Marketplace extensions
run as local child processes with the user's privileges. This is intended — but
the *reach* is easy to underestimate: the bundled Filesystem extension is
configured with a root of `/`, so installing it gives the agent read and write
access to the entire filesystem. A bug that installs, enables, or reconfigures
an extension **without explicit user consent**, or that lets extension arguments
be influenced by untrusted input, is a serious finding.

**Prompt injection is a real boundary, not a curiosity.** GIAP runs an agent
with tool access. Content the agent reads — web pages, documents, tool output,
device names, extension responses — is untrusted data, never instructions. A
path where injected content causes a state-changing tool call (controlling a
device, writing memory, installing an extension, exfiltrating a secret) without
the user's intent is in scope, and is more valuable to us than most memory-safety
reports.

**Sensitive data stays on the device, and can be encrypted at rest.** Face-recognition
enrolments, camera event detections, microphone audio, chat history, saved conversation state and
memory fragments live in the pond's data directory (`pond_system.db`, `pond_logs.db`, Goose's
sessions and prompt cache). Secrets live in `secrets.json`, encrypted with XChaCha20-Poly1305 under
a `master.key` kept beside it unless `POND_SECRET_KEY_FILE` moves it. On a Linux pond,
`giap.sh data-encrypt` puts the whole private part of the data directory in a LUKS2 volume whose
key is sealed to the board's TPM, with the household's admin card as the recovery key; `giap.sh
doctor` warns while a pond's data is not encrypted
([data-volume-encryption.md](../docs/architecture/data-volume-encryption.md)). On a board whose
security fuses are not burned, the TPM's secret is not guaranteed to be unique, so the volume may
open with the storage moved into another unfused Jetson; that is documented and reported by the
doctor, and is not a finding on its own. These are in scope: a way to read the volume's contents
without the board's TPM or the card; a pond that writes private data outside the volume, or starts
on the bare directory while the volume is closed; and the admin card or a volume key appearing in
an argument list, a log or the event log. Anything that moves this data **off the device**, or
exposes it through the API to a client that should not see it, very much is too: inference, voice,
and memory run on your hardware. Outbound calls go through the egress gate, are recorded in the
egress log, and are refused under `network_mode = offline`; one that bypasses the gate is a finding.

**Compromise reaches the physical world.** GIAP drives Matter devices. Assess
severity with that in mind: an authorization bypass here can unlock a door, not
just leak a row.

---

## What we already run

Reports that simply restate this tooling's output are usually not actionable on
their own. The `Security` workflow runs on every PR and push to `main`, weekly,
and on demand:

- **`cargo audit`** against the workspace lockfile.
- **`npm audit --audit-level=high`** against every npm lockfile we build.
- **`osv-scanner`** across all lockfiles, including `native/pondnet/go.mod`. This exists because `cargo audit`
  reads RustSec while Dependabot reads the GitHub Advisory Database, and the two
  disagree on affected ranges — a green `cargo audit` is not evidence a lockfile
  is clean. OSV aggregates both.
- **`gitleaks`** over the commits a pull request adds, and over the full git
  history on pushes to `main` and the weekly run, so a credential committed and
  later "deleted" still fails the build.
- **Dependabot** alerts, with grouped update PRs configured in
  `.github/dependabot.yml`.

### Known accepted risks

`.cargo/audit.toml` and `osv-scanner.toml` are the registers of advisories we have consciously accepted,
each with a written justification and the condition for removing it. Its own
triage rule is that anything fixable from our lockfile gets fixed, never
ignored. Current entries are advisories with no fixed release, or ones owned by
an upstream dependency we cannot move — for example a crate that appears in a
lockfile but is never linked into a shipped binary.

If you believe an entry there is wrong — the reasoning does not hold, or the
code *is* reachable — that is a legitimate report and we would like to know.

---

## Hardening a deployment

Not vulnerabilities, but the difference between a safe and an unsafe install:

- **Put GIAP on a trusted network segment.** The HTTPS companion listener binds
  every IPv4 interface by design. Do not port-forward it to the internet or place
  it on a guest or shared VLAN; use the built-in remote access for phones away from
  home.
- **Only install extensions you trust**, and read the command and arguments
  before confirming. Treat installing one as running that code yourself.
- **Encrypt the pond's data** with `bash scripts/giap.sh data-encrypt` (Linux;
  `--dry-run` first). Until then, biometric enrolments and chat history are plain files
  that anyone holding the storage can read. Keep the printed admin card safe: it is the
  volume's recovery key. On a Mac, turn on FileVault.
- **Treat an unfused Jetson as protecting a copied disk, not a moved one.** Until the
  board's security fuses are burned, its TPM may open the volume with the storage in
  another unfused Jetson.
- **Keep the host patched**, and pull `main` regularly — there is no
  auto-update.
- **Re-pair rather than share.** Each client should hold its own session; do not
  copy tokens between devices. Remove a lost phone from Devices: that revokes its
  sessions and its remote access.
- **Keep the setup card private.** Until the Pond is claimed, whoever holds the
  card's code can claim it.

---

## Credits

We publicly thank everyone who reports a vulnerability in good faith, unless
they ask us not to. Reporters are credited in the advisory and in the release
notes for the fix.

[advisory]: https://github.com/jarida-io/goose-in-a-pond/security/advisories/new
[goose]: https://github.com/aaif-goose/goose
