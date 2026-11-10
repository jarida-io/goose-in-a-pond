#!/usr/bin/env bash
# Tests for go-setup.sh. Run: bash scripts/lib/go-setup.test.sh   (also under /bin/bash 3.2)
#
# Everything runs in a scratch directory with its own HOME and a PATH built from symlinks to only the
# tools the code needs, so no real Go or home directory is read or changed. The download site is a
# directory of fixtures reached through file://, so the download, checksum and unpack logic runs for
# real without the network.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/go-setup-test.XXXXXX")" || exit 1
[ -n "$SCRATCH" ] && [ "$SCRATCH" != "$PWD" ] && [ -d "$SCRATCH" ] || exit 1
SCRATCH="$(cd "$SCRATCH" && pwd)"
trap '[ -n "${SCRATCH:-}" ] && [ "$SCRATCH" != "$PWD" ] && [ -d "$SCRATCH" ] && rm -rf "$SCRATCH"' EXIT

PASSED=0; FAILED=0
# t <name> <function>: the function runs in a subshell, so environment changes never leak.
t() {
  local name="$1" fn="$2" out rc
  out="$( ( "$fn" ) 2>&1 )"; rc=$?
  if [ "$rc" -eq 0 ]; then PASSED=$((PASSED + 1)); else FAILED=$((FAILED + 1)); printf 'FAIL: %s\n' "$name"; [ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/      /'; fi
}
eq() { [ "$1" = "$2" ] || { printf 'expected [%s] got [%s]\n' "$2" "$1"; return 1; }; }

# A directory of symlinks to the real tools the code needs, and nothing else: no go on it.
TOOLS="$SCRATCH/tools"; mkdir -p "$TOOLS"
for tool in curl tar gzip mktemp ln mv rm mkdir cat tr head cut grep sed awk dirname uname sha256sum shasum chmod printf sh; do
  p="$(command -v "$tool" 2>/dev/null)" && [ -n "$p" ] && [ -x "$p" ] && ln -sf "$p" "$TOOLS/$tool"
done

# giap.sh's reporting, reduced to what a test reads.
ok()      { printf 'ok: %s\n' "$*"; }
warn()    { printf 'warn: %s\n' "$*"; }
bad()     { printf 'bad: %s\n' "$*"; }
info()    { printf 'info: %s\n' "$*"; }
note()    { printf 'note: %s\n' "$*"; }
confirm() { [ "${ANSWER:-yes}" = yes ]; }

# A fresh world per test: its own HOME, a PATH with no go, a repo whose go.mod pins 1.27.3, and a
# download site holding a Go 1.27.3 for this platform with its published checksum.
world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/home" "$W/bin" "$W/repo/native/pondnet" "$W/site"
  export HOME="$W/home" PATH="$W/bin:$TOOLS" REPO_ROOT="$W/repo"
  export GIAP_GO_HOME="$W/home/.giap/go" GIAP_GO_DL_URL="file://$W/site"
  unset DRY_RUN ANSWER
  printf 'module example.com/pondnet\n\ngo 1.27.3\n\nrequire (\n\tgolang.org/x/net v0.62.0\n)\n' > "$W/repo/native/pondnet/go.mod"
  # shellcheck source=go-setup.sh
  source "$HERE/go-setup.sh"
}
# A fake go that answers `go version`, at <dir>/go.
# shellcheck disable=SC2016  # the fake go's own $1, expanded when it runs
mk_go() { mkdir -p "$1"; printf '#!/bin/sh\n[ "$1" = version ] && echo "go version go%s linux/arm64"\n' "$2" > "$1/go"; chmod +x "$1/go"; }
# The release tarball the site serves, and its .sha256 (or a wrong one).
mk_release() {
  local ver="$1" sum="${2:-}" file platform
  platform="$(go_platform)"; file="go${ver}.${platform}.tar.gz"
  mk_go "$W/pkg/go/bin" "$ver"
  tar -czf "$W/site/$file" -C "$W/pkg" go
  [ -n "$sum" ] || sum="$(go_sha256 "$W/site/$file")"
  printf '%s\n' "$sum" > "$W/site/$file.sha256"
}

# ── the pin ──────────────────────────────────────────────────────────────────

t_reads_the_go_line() { world; eq "$(go_required_version)" "1.27.3"; }
t_prefers_the_toolchain_line() {
  world
  printf 'module m\n\ngo 1.25\n\ntoolchain go1.27.3\n' > "$W/repo/native/pondnet/go.mod"
  eq "$(go_required_version)" "1.27.3"
}
t_no_go_mod_is_a_failure() { world; rm "$W/repo/native/pondnet/go.mod"; ! go_required_version >/dev/null; }

t_versions_compare_by_number() {
  world
  go_version_ok 1.27.3 1.27.3 || { echo "the pin itself"; return 1; }
  go_version_ok 1.27.10 1.27.3 || { echo "1.27.10 is newer than 1.27.3"; return 1; }
  go_version_ok 1.28 1.27.3 || { echo "1.28 is newer"; return 1; }
  go_version_ok 1.28rc1 1.27.3 || { echo "a release candidate counts as its release"; return 1; }
  ! go_version_ok 1.18.1 1.27.3 || { echo "the Jetson's apt Go is too old"; return 1; }
  ! go_version_ok 1.27.2 1.27.3 || { echo "a patch below the pin is too old"; return 1; }
  ! go_version_ok "" 1.27.3
}

# ── finding one ──────────────────────────────────────────────────────────────

t_an_old_go_on_path_is_passed_over_for_a_recorded_one() {
  world
  mk_go "$W/bin" 1.18.1
  mk_go "$W/other" 1.27.3
  go_record "$W/other"
  eq "$(go_find_ok 1.27.3)" "1.27.3|$W/other"
}
t_none_found_is_a_failure() { world; mk_go "$W/bin" 1.18.1; ! go_find_ok 1.27.3 >/dev/null; }
t_use_repo_puts_it_first_on_path() {
  world
  mk_go "$W/other" 1.27.3; go_record "$W/other"
  go_use_repo >/dev/null || return 1
  eq "$(go_version_of "$(command -v go)")" "1.27.3"
}

# ── downloading one ──────────────────────────────────────────────────────────

t_downloads_verifies_and_records_the_pin() {
  world
  mk_release 1.27.3
  mk_go "$W/bin" 1.18.1
  go_ensure >/dev/null || return 1
  eq "$(go_version_of "$(go_home)/current/bin/go")" "1.27.3" || return 1
  eq "$(head -1 "$(go_record_file)")" "$(go_home)/go1.27.3/bin"
}
t_a_checksum_mismatch_installs_nothing() {
  world
  mk_release 1.27.3 0000000000000000000000000000000000000000000000000000000000000000
  go_download 1.27.3 >/dev/null 2>&1; local rc=$?
  eq "$rc" 3 || return 1
  [ ! -e "$(go_home)/go1.27.3" ] || { echo "a half install was left behind"; return 1; }
}
t_declining_downloads_nothing() {
  world
  mk_release 1.27.3
  ANSWER=no go_ensure >/dev/null && { echo "should fail when declined"; return 1; }
  [ ! -e "$(go_home)/current" ]
}
t_an_installed_one_is_used_without_a_download() {
  world
  mk_go "$W/home/.giap/go/current/bin" 1.27.3
  out="$(go_ensure)" || return 1
  case "$out" in *"nothing to download"*) ;; *) echo "$out"; return 1 ;; esac
}
t_dry_run_touches_nothing() {
  world
  mk_release 1.27.3
  DRY_RUN=true go_ensure >/dev/null || return 1
  [ ! -e "$(go_home)/current" ]
}

for fn in $(declare -F | awk '{print $3}' | grep '^t_'); do t "$fn" "$fn"; done
printf '%s passed, %s failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
