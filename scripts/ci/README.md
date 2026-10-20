# Private Cargo dependency access in CI

The Rust job fetches the private `jarida-io/llama-cpp-rs-giap` dependency before
building. A repository or organization Actions secret named `CARGO_GITHUB_TOKEN`
must hold a fine-grained token with read-only Contents access to that repository.
The default `GITHUB_TOKEN` cannot read a different private repository.
Both Rust and live-test jobs also accept the existing `GIAP_PRIVATE_FORK_TOKEN`
name as a fallback; new configuration should use `CARGO_GITHUB_TOKEN`.

The credential is available only to the fetch step. `git-askpass.sh` answers
only prompts for that exact HTTPS repository path; it fails closed for other
hosts and paths. Git credential persistence is disabled for that step, and the
token is not embedded in URLs or written into Git configuration. Cargo's CLI
Git fetching is required so the helper is honored.

Fork PR workflows cannot access repository secrets. A maintainer must review
such changes before testing them on a trusted repository branch. This workflow
continues to use `pull_request`, never `pull_request_target`.

On a fork PR the Rust job is therefore skipped rather than run to a certain
failure. GitHub reports a skipped job as a success, so `Rust — skipped on a fork
PR` fails in its place whenever the PR changes Rust (a `.rs` file, a Cargo
manifest or lock, `crates/`, `vendor/`, `.cargo/`, `Cross.toml` or the goose
submodule), and passes otherwise. Make both checks required together. To merge a
fork PR that touches Rust, push its branch to `jarida-io/goose-in-a-pond` so the
job runs with the secret. Pushes and same-repository PRs always run the job, and it fails
loudly there when the secret is missing. Claude Code Review skips fork PRs for
the same reason, since they get no OIDC token either. Comment `@claude review`
to request a review instead.

Run `python3 scripts/ci/test_git_askpass.py` and
`bash -n scripts/ci/git-askpass.sh` to validate routing with synthetic data.
The tests never consume an existing shell credential. Successful local tests
do not prove that GitHub's secret exists, is authorized by the organization, or
has permission to read the dependency; the hosted fetch step establishes that.

The CI pull-request event covers stacked PRs as well as main-targeted PRs so
security dependency stacks receive the same checks before their bases merge.

Security scans also run for stacked PRs and explicitly cover the Goose
standalone lockfile and Matter lockfile, which previously fell outside the
OSV job. The integration branch includes the separately reviewed Matter fix
so the expanded scanner validates the complete set of remediations.

## Claude reviews on pull requests

Two workflows comment on each same-repository PR when it is opened, reopened or
marked ready. Both use the `CLAUDE_CODE_OAUTH_TOKEN` secret and skip fork PRs.

- `claude-code-review.yml` asks whether the change is correct.
- `claude-security-review.yml` asks what an attacker could do with it. It covers
  the ground between the scanners: an auth check a new route skips, a path or URL
  a handler trusts, a tool added to `DIRECT_DISPATCH_ALLOWLIST`. It reports only
  findings tied to a line of the diff, comments, and never fails the PR, because a
  model's reading is not proof.

Neither runs again on later pushes. Ask for a fresh pass with an `@claude` comment,
which `claude.yml` handles.
