# shellcheck shell=bash
# The checks CI runs, run on this machine: `bash scripts/giap.sh check`.
#
#   bash scripts/giap.sh check                # the areas this branch changes, against main
#   bash scripts/giap.sh check desktop rust   # named areas: rust desktop go shell matter music
#   bash scripts/giap.sh check --all          # every area
#   bash scripts/giap.sh check --e2e          # desktop also runs Playwright (about 12 minutes)
#   bash scripts/giap.sh check --full         # rust also checks the production binary (compiles Goose)
#   bash scripts/giap.sh check --base REF     # compare against REF instead of main
#   bash scripts/giap.sh check --dry-run      # print the commands, run nothing
#
# Sourced by giap.sh and local-checks.test.sh. The Rust crate lists are read from ci.yml, so they
# cannot drift from CI. Every area runs even after one fails; the exit status is 1 if any failed.

LC_AREAS="rust desktop go shell matter music"
LC_PRIVATE_FORK="https://github.com/jarida-io/llama-cpp-rs-giap"

lc_usage() {
  sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

# lc_ci_crates <ci.yml> <step name>: the crates that ci.yml step passes with `-p`, one per line.
lc_ci_crates() {
  awk -v step="name: $2" '
    index($0, step) { on = 1; next }
    on && /^ *- name:/ { exit }
    on { for (i = 1; i < NF; i++) if ($i == "-p") print $(i + 1) }
  ' "$1"
}

# lc_area_of <path>: the area a changed file belongs to; nothing for docs and the rest.
lc_area_of() {
  case "$1" in
    *.md) ;;
    crates/*|vendor/*|Cargo.toml|Cargo.lock|.cargo/*|goose) echo rust ;;
    pond-desktop/*) echo desktop ;;
    native/pondnet/*) echo go ;;
    matter-server/*) echo matter ;;
    extensions/music/*) echo music ;;
    *.sh) echo shell ;;
  esac
}

# lc_changed_areas <base ref>: each area the branch touches, once, in LC_AREAS order.
lc_changed_areas() {
  local base="$1" fork_point touched="" area f
  fork_point="$(git merge-base HEAD "$base")" || return 1
  while IFS= read -r f; do
    area="$(lc_area_of "$f")"
    case " $touched " in *" $area "*) ;; *) touched="$touched $area" ;; esac
  done <<EOF
$(git diff --name-only "$fork_point"; git ls-files --others --exclude-standard)
EOF
  for area in $LC_AREAS; do
    case " $touched " in *" $area "*) printf '%s\n' "$area" ;; esac
  done
}

# lc_default_base: the first of upstream/main, origin/main and main that exists.
lc_default_base() {
  local ref
  for ref in upstream/main origin/main main; do
    if git rev-parse --verify --quiet "$ref" >/dev/null; then printf '%s\n' "$ref"; return 0; fi
  done
  return 1
}

# lc_run <command…>: print it, then run it unless DRY_RUN is true.
lc_run() {
  printf '$ %s\n' "$*"
  [ "${DRY_RUN:-false}" = true ] && return 0
  "$@"
}

# lc_need <tool> <hint>: fail with how to get the tool when it is missing.
lc_need() {
  [ "${DRY_RUN:-false}" = true ] && return 0
  command -v "$1" >/dev/null 2>&1 && return 0
  printf '%s is not installed. %s\n' "$1" "$2" >&2
  return 1
}

lc_check_rust() {
  local full="$1" ci="$LC_REPO/.github/workflows/ci.yml" clippy test
  lc_need cargo "Install Rust: https://rustup.rs" || return 1
  if [ "${DRY_RUN:-false}" != true ] \
    && ! GIT_TERMINAL_PROMPT=0 git ls-remote --exit-code "$LC_PRIVATE_FORK" HEAD >/dev/null 2>&1; then
    printf '%s\n' \
      "Rust needs read access to the private jarida-io/llama-cpp-rs-giap repository, and this" \
      "machine's git cannot read it (docs/CONTRIBUTING.md, Prerequisites). Ask a maintainer for" \
      "access, or work on the dashboard, which needs none: bash scripts/giap.sh check desktop" >&2
    return 1
  fi
  clippy="$(lc_ci_crates "$ci" "cargo clippy (fast crates)" | sed 's/^/-p /' | tr '\n' ' ')"
  test="$(lc_ci_crates "$ci" "cargo test (fast crates)" | sed 's/^/-p /' | tr '\n' ' ')"
  if [ -z "$clippy" ] || [ -z "$test" ]; then
    printf 'Could not read the fast-crate lists from %s; has a step been renamed?\n' "$ci" >&2
    return 1
  fi
  lc_run cargo fmt --check || return 1
  # shellcheck disable=SC2086  # each list is many `-p crate` words
  lc_run cargo clippy --locked $clippy || return 1
  # shellcheck disable=SC2086
  lc_run cargo test --locked $test || return 1
  [ "$full" = true ] || return 0
  lc_run cargo check --locked -p pond-server -p pond-adapters-goose --all-targets || return 1
  lc_run cargo check --locked -p pond-server --no-default-features --features goose-agent,local-inference,vision-onnx,face-onnx --all-targets
}

lc_check_desktop() {
  local e2e="$1"
  lc_need npm "Run: bash scripts/giap.sh node" || return 1
  (
    cd "$LC_REPO/pond-desktop" || exit 1
    # A missing or older install than the lockfile fails in ways that look like code errors.
    if [ ! -f node_modules/.package-lock.json ] || [ package-lock.json -nt node_modules/.package-lock.json ]; then
      lc_run npm ci --no-audit --no-fund || exit 1
    fi
    lc_run npm run typecheck || exit 1
    lc_run npm run lint || exit 1
    lc_run npm test || exit 1
    [ "$e2e" = true ] || exit 0
    lc_run npx playwright install chromium || exit 1
    lc_run npx playwright test
  )
}

lc_check_go() {
  lc_need go "Install Go (version in native/pondnet/go.mod): https://go.dev/dl" || return 1
  (
    cd "$LC_REPO/native/pondnet" || exit 1
    printf '$ gofmt -l .\n'
    if [ "${DRY_RUN:-false}" != true ] && [ -n "$(gofmt -l .)" ]; then
      gofmt -l . >&2
      exit 1
    fi
    lc_run go test -race ./... || exit 1
    lc_run go vet ./...
  )
}

lc_check_shell() {
  lc_need shellcheck "Install it (CI pins 0.11.0): https://github.com/koalaman/shellcheck#installing" || return 1
  (
    cd "$LC_REPO" || exit 1
    failed=0
    printf '$ shellcheck --severity=warning <every tracked *.sh outside goose/>\n'
    if [ "${DRY_RUN:-false}" != true ]; then
      git ls-files -z '*.sh' ':!:goose/' | xargs -0 shellcheck --severity=warning || failed=1
    fi
    for test in scripts/lib/*.test.sh; do
      lc_run bash "$test" || failed=1
    done
    exit "$failed"
  )
}

lc_check_npm_package() {
  local dir="$1"; shift
  lc_need npm "Run: bash scripts/giap.sh node" || return 1
  (
    cd "$LC_REPO/$dir" || exit 1
    lc_run npm ci --no-audit --no-fund || exit 1
    local step
    for step in "$@"; do
      # shellcheck disable=SC2086  # each step is a command line
      lc_run $step || exit 1
    done
  )
}

lc_check_area() {
  case "$1" in
    rust) lc_check_rust "$LC_FULL" ;;
    desktop) lc_check_desktop "$LC_E2E" ;;
    go) lc_check_go ;;
    shell) lc_check_shell ;;
    matter) lc_check_npm_package matter-server "npx tsc --noEmit" "npx vitest run" ;;
    music) lc_check_npm_package extensions/music "npm run typecheck" "npm test" ;;
  esac
}

lc_main() {
  local areas="" all=false base="" area passed="" failed=""
  LC_E2E=false; LC_FULL=false
  LC_REPO="${LC_REPO:-$(git rev-parse --show-toplevel)}"
  while [ $# -gt 0 ]; do
    case "$1" in
      --all) all=true ;;
      --e2e) LC_E2E=true ;;
      --full) LC_FULL=true ;;
      --dry-run) DRY_RUN=true ;;
      --base) [ $# -ge 2 ] || { echo "check: --base needs a ref" >&2; return 2; }; base="$2"; shift ;;
      -h|--help) lc_usage; return 0 ;;
      rust|desktop|go|shell|matter|music) areas="$areas $1" ;;
      *) printf "check: unknown argument '%s'\n\n" "$1" >&2; lc_usage >&2; return 2 ;;
    esac
    shift
  done

  if [ "$all" = true ]; then
    areas="$LC_AREAS"
  elif [ -z "$areas" ]; then
    base="${base:-$(lc_default_base)}" || { echo "check: no main branch to compare with; name areas or pass --base" >&2; return 2; }
    areas="$(cd "$LC_REPO" && lc_changed_areas "$base")" || { echo "check: cannot compare with $base" >&2; return 2; }
    if [ -z "$areas" ]; then
      echo "Nothing to check: this branch changes no code CI tests against $base."
      return 0
    fi
    echo "Checking what this branch changes against $base:" $areas
  fi

  for area in $areas; do
    printf '\n== %s\n' "$area"
    if lc_check_area "$area"; then passed="$passed $area"; else failed="$failed $area"; fi
  done

  printf '\n'
  if [ "${DRY_RUN:-false}" = true ]; then
    echo "Dry run: nothing was run."
    return 0
  fi
  [ -n "$passed" ] && printf 'Passed:%s\n' "$passed"
  [ -n "$failed" ] && { printf 'Failed:%s\n' "$failed"; return 1; }
  return 0
}
