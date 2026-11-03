#!/usr/bin/env bash
# Tests for local-checks.sh. Run: bash scripts/lib/local-checks.test.sh   (also under /bin/bash 3.2)
#
# No network and no build. The crate lists are read from the real ci.yml; changed-file detection
# runs in a scratch git repository; the Rust access check runs against a fake git and cargo.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/local-checks-test.XXXXXX")" || exit 1
[ -n "$SCRATCH" ] && [ "$SCRATCH" != "$PWD" ] && [ -d "$SCRATCH" ] || exit 1
SCRATCH="$(cd "$SCRATCH" && pwd)"
trap '[ -n "${SCRATCH:-}" ] && [ "$SCRATCH" != "$PWD" ] && [ -d "$SCRATCH" ] && rm -rf "$SCRATCH"' EXIT

# shellcheck source=local-checks.sh
source "$HERE/local-checks.sh"

PASSED=0; FAILED=0
# t <name> <function>: the function runs in a subshell, so environment changes never leak.
t() {
  local name="$1" fn="$2" out rc
  out="$( ( "$fn" ) 2>&1 )"; rc=$?
  if [ "$rc" -eq 0 ]; then PASSED=$((PASSED + 1))
  else
    FAILED=$((FAILED + 1)); printf 'FAIL: %s\n' "$name"
    [ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/      /'
  fi
}
eq()       { [ "$1" = "$2" ] || { printf 'expected [%s] got [%s]\n' "$2" "$1"; return 1; }; }
contains() { case "$1" in *"$2"*) return 0 ;; *) printf 'expected to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; esac; }
lacks()    { case "$1" in *"$2"*) printf 'expected NOT to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; *) return 0 ;; esac; }

CI="$REPO/.github/workflows/ci.yml"

test_reads_both_fast_crate_lists_from_ci() {
  local clippy test
  clippy="$(lc_ci_crates "$CI" "cargo clippy (fast crates)" | tr '\n' ' ')"
  test="$(lc_ci_crates "$CI" "cargo test (fast crates)" | tr '\n' ' ')"
  contains "$clippy" "pond-core " && contains "$test" "pond-core " || return 1
  # mistral.rs is linted but its tests are not in the fast pass.
  contains "$clippy" "pond-adapters-mistralrs" && lacks "$test" "pond-adapters-mistralrs"
}

test_a_step_list_ends_at_the_next_step() {
  cat >"$SCRATCH/ci.yml" <<'EOF'
      - name: cargo test (fast crates)
        run: |
          cargo test --locked \
            -p alpha \
            -p beta
      - name: something else
        run: cargo check -p gamma
EOF
  eq "$(lc_ci_crates "$SCRATCH/ci.yml" "cargo test (fast crates)" | tr '\n' ' ')" "alpha beta "
}

test_files_map_to_their_area() {
  eq "$(lc_area_of crates/pond-api/src/routes.rs)" rust || return 1
  eq "$(lc_area_of Cargo.lock)" rust || return 1
  eq "$(lc_area_of goose)" rust || return 1
  eq "$(lc_area_of pond-desktop/src/App.tsx)" desktop || return 1
  eq "$(lc_area_of native/pondnet/proxy.go)" go || return 1
  eq "$(lc_area_of scripts/lib/node-setup.sh)" shell || return 1
  eq "$(lc_area_of matter-server/src/controller.ts)" matter || return 1
  eq "$(lc_area_of extensions/music/index.ts)" music || return 1
  eq "$(lc_area_of crates/pond-api/README.md)" "" || return 1
  eq "$(lc_area_of docs/api.md)" ""
}

# A scratch repository on `main` with one commit, then a feature branch.
scratch_repo() {
  cd "$SCRATCH" && rm -rf repo && mkdir repo && cd repo || return 1
  git init -q -b main && git config user.email t@example.com && git config user.name t
  mkdir -p pond-desktop/src docs && echo a >pond-desktop/src/a.ts && echo a >docs/a.md
  git add -A && git commit -qm base && git checkout -qb feature
}

test_changed_areas_are_what_the_branch_touches_once_each() {
  scratch_repo || return 1
  echo b >pond-desktop/src/a.ts && echo b >docs/a.md && git commit -qam change
  echo c >"pond-desktop/src/with space.ts"
  echo c >pond-desktop/src/b.ts
  eq "$(lc_changed_areas main)" desktop
}

test_a_docs_only_branch_has_nothing_to_check() {
  scratch_repo || return 1
  echo b >docs/a.md && git commit -qam docs
  local out rc
  out="$(LC_REPO="$PWD" lc_main --base main)"; rc=$?
  eq "$rc" 0 && contains "$out" "Nothing to check"
}

test_a_dry_run_prints_the_commands_and_runs_nothing() {
  local out rc
  out="$(LC_REPO="$REPO" lc_main desktop go --e2e --dry-run)"; rc=$?
  eq "$rc" 0 || return 1
  contains "$out" '$ npm run lint' && contains "$out" '$ npx playwright test' \
    && contains "$out" '$ go vet ./...' && contains "$out" "Dry run: nothing was run."
}

test_full_adds_the_production_binary() {
  local out
  out="$(LC_REPO="$REPO" lc_main rust --dry-run)"
  lacks "$out" "-p pond-server" || return 1
  out="$(LC_REPO="$REPO" lc_main rust --full --dry-run)"
  contains "$out" '$ cargo check --locked -p pond-server -p pond-adapters-goose --all-targets'
}

test_an_unknown_argument_is_refused() {
  local rc
  LC_REPO="$REPO" lc_main rsut >/dev/null 2>&1; rc=$?
  eq "$rc" 2
}

test_every_area_runs_after_one_fails() {
  lc_check_area() { [ "$1" != go ]; }
  local out rc
  out="$(LC_REPO="$REPO" lc_main go shell)"; rc=$?
  eq "$rc" 1 && contains "$out" "Passed: shell" && contains "$out" "Failed: go"
}

test_without_access_to_the_private_fork_rust_says_what_to_do_and_builds_nothing() {
  mkdir -p "$SCRATCH/bin"
  cat >"$SCRATCH/bin/git" <<'EOF'
#!/usr/bin/env bash
[ "$1" = ls-remote ] && exit 128
exit 0
EOF
  cat >"$SCRATCH/bin/cargo" <<EOF
#!/usr/bin/env bash
echo ran >"$SCRATCH/cargo-ran"
EOF
  chmod +x "$SCRATCH/bin/git" "$SCRATCH/bin/cargo"
  rm -f "$SCRATCH/cargo-ran"
  local out rc
  out="$(PATH="$SCRATCH/bin:$PATH" LC_REPO="$REPO" lc_main rust 2>&1)"; rc=$?
  eq "$rc" 1 || return 1
  contains "$out" "jarida-io/llama-cpp-rs-giap" && contains "$out" "check desktop" || return 1
  [ ! -e "$SCRATCH/cargo-ran" ] || { echo "cargo ran without access to the private fork"; return 1; }
}

for fn in $(declare -F | awk '{print $3}' | grep '^test_'); do
  t "$fn" "$fn"
done
printf '%d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
