#!/usr/bin/env bash
# Tests for node-setup.sh. Run: bash scripts/lib/node-setup.test.sh   (also under /bin/bash 3.2)
#
# Everything runs in a scratch directory with its own HOME and a PATH built from symlinks to only the
# tools the code needs, so no real Node, nvm or home directory is read or changed, and the result is
# the same on a developer's machine and on CI. nodejs.org is a directory of fixtures reached through
# file://, so the download, checksum and unpack logic runs for real without the network.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/node-setup-test.XXXXXX")" || exit 1
[ -n "$SCRATCH" ] && [ "$SCRATCH" != "$PWD" ] && [ -d "$SCRATCH" ] || exit 1
SCRATCH="$(cd "$SCRATCH" && pwd)"
trap '[ -n "${SCRATCH:-}" ] && [ "$SCRATCH" != "$PWD" ] && [ -d "$SCRATCH" ] && rm -rf "$SCRATCH"' EXIT

ORIG_PATH="$PATH"     # the real PATH, for tests that run giap.sh and so need git, sed and the rest
PASSED=0; FAILED=0
# t <name> <function>: the function runs in a subshell, so environment changes never leak.
t() {
  local name="$1" fn="$2" out rc
  out="$( ( "$fn" ) 2>&1 )"; rc=$?
  if [ "$rc" -eq 0 ]; then PASSED=$((PASSED + 1)); else FAILED=$((FAILED + 1)); printf 'FAIL: %s\n' "$name"; [ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/      /'; fi
}
eq()       { [ "$1" = "$2" ] || { printf 'expected [%s] got [%s]\n' "$2" "$1"; return 1; }; }
contains() { case "$1" in *"$2"*) return 0 ;; *) printf 'expected to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; esac; }
lacks()    { case "$1" in *"$2"*) printf 'expected NOT to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; *) return 0 ;; esac; }
# names <dir>: the entries of <dir>, dot files included, one per line -- what `ls -A` lists, but
# from a glob, so a file name reaches grep intact.
names()    { local f; for f in "$1"/* "$1"/.[!.]* "$1"/..?*; do { [ -e "$f" ] || [ -L "$f" ]; } && printf '%s\n' "${f##*/}"; done; }

# A directory of symlinks to the real tools the code needs, and nothing else: no node on it. gzip
# because GNU tar runs it for `-z`, where macOS's tar decompresses by itself.
TOOLS="$SCRATCH/tools"; mkdir -p "$TOOLS"
for tool in curl tar gzip mktemp ln mv rm mkdir cat tr head cut grep sed awk dirname basename uname sha256sum shasum chmod printf sort wc env bash sh date touch ls; do
  p="$(command -v "$tool" 2>/dev/null)" && [ -n "$p" ] && [ -x "$p" ] && ln -sf "$p" "$TOOLS/$tool"
done

# A fresh world per test: its own HOME, PATH with no node, and the repo root pointed at a scratch copy.
world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/home" "$W/bin" "$W/repo"
  export HOME="$W/home" PATH="$W/bin:$TOOLS" REPO_ROOT="$W/repo"
  export GIAP_NODE_HOME="$W/home/.giap/node"
  unset NVM_DIR FNM_DIR VOLTA_HOME ASDF_DATA_DIR GIAP_NODE_DIST_URL DRY_RUN
  # shellcheck source=node-setup.sh
  source "$HERE/node-setup.sh"
}
# A fake node that answers --version and -v, at <dir>/node.
mk_node() { mkdir -p "$1"; printf '#!/bin/sh\ncase "$1" in --version|-v) echo %s ;; esac\n' "$2" > "$1/node"; chmod +x "$1/node"; }

# ── the range ────────────────────────────────────────────────────────────────

t_range_inside() {
  world
  for v in v22.13.0 22.13.0 v22.13.1 v22.99.9 v24.0.0 v24.11.0 v26.0.0 v26.10.0 v27.1.0 v30.0.0; do
    node_version_ok "$v" || { echo "$v should be inside"; return 1; }
  done
}
t_range_outside() {
  world
  for v in v20.19.0 v21.0.0 v22.11.9 v22.12.0 v22.12.9 v22.0.0 v23.5.0 v25.8.2 v18.0.0 "" garbage v 22; do
    ! node_version_ok "$v" || { echo "[$v] should be outside"; return 1; }
  done
}
t_ui_range() {
  world
  # Vite's floor is lower than the repo's: 20.19 builds the UI and is still outside the range.
  for v in v20.19.0 v20.20.1 v22.12.0 v22.14.0 v23.5.0 v24.1.0 v25.8.2 v26.0.0; do
    node_version_ui_ok "$v" || { echo "$v should be able to build the UI"; return 1; }
  done
  for v in v20.18.9 v20.0.0 v21.7.0 v22.11.9 v18.20.0 "" garbage; do
    ! node_version_ui_ok "$v" || { echo "[$v] should not"; return 1; }
  done
  node_version_ui_ok v20.19.0 && ! node_version_ok v20.19.0
}
t_ranges_nest() {
  world
  # Anything the repo's range accepts can build the UI and run matter.js.
  for v in v22.13.0 v22.30.0 v24.0.0 v24.9.9 v26.0.0 v28.1.0; do
    node_version_ok "$v" && node_version_ui_ok "$v" && node_version_matter_ok "$v" \
      || { echo "$v breaks the nesting"; return 1; }
  done
  # 22.12.x builds the UI and is outside both the repo's range and matter.js 0.17's.
  for v in v22.12.0 v22.12.9; do
    node_version_ui_ok "$v" && ! node_version_ok "$v" && ! node_version_matter_ok "$v" \
      || { echo "$v is misjudged"; return 1; }
  done
}
t_matter_floor() {
  world
  # matter.js 0.17: >=20.19.0 <22.0.0 || >=22.13.0 (the same range server_setup.rs enforces).
  for v in v20.19.0 v20.20.1 v21.0.0 v21.7.3 v22.13.0 v22.14.1 v23.5.0 v24.0.0 v25.8.2 v26.10.0; do
    node_version_matter_ok "$v" || { echo "$v should do for matter.js"; return 1; }
  done
  for v in v20.18.9 v18.20.0 v20.0.0 v22.0.0 v22.5.1 v22.12.0 v22.12.9 v12.22.9 "" garbage; do
    ! node_version_matter_ok "$v" || { echo "[$v] should not"; return 1; }
  done
}
t_why_not_matter() {
  world
  contains "$(node_why_not_matter v22.12.1)" "excludes Node 22.0 through 22.12 (this is 22.12)" || return 1
  contains "$(node_why_not_matter v22.12.1)" "22.13 or newer" || return 1
  contains "$(node_why_not_matter v12.22.9)" "needs Node 20.19 or newer" || return 1
  contains "$(node_why_not_matter v20.18.0)" "needs Node 20.19 or newer" || return 1
  contains "$(node_why_not_matter garbage)" "no node" || return 1
  eq "$(node_why_not_matter v24.1.0)" "" || return 1
}
t_node_22_12_is_outside_the_range() {
  world; stubs
  mk_node "$W/bin" v22.12.1
  node_report; [ "$?" -ne 0 ] || return 1
  contains "$(logged)" "node v22.12.1 on PATH is outside ^22.13.0" || return 1
  contains "$(logged)" "older than 22.13"
}
t_why_not() {
  world
  contains "$(node_why_not v25.8.2)" "odd-numbered" && contains "$(node_why_not v25.8.2)" "vitest" || return 1
  contains "$(node_why_not v23.1.0)" "odd-numbered" || return 1
  contains "$(node_why_not v20.19.0)" "22.13 or newer" || return 1
  contains "$(node_why_not v22.12.0)" "older than 22.13" || return 1
  contains "$(node_why_not v16.0.0)" "older" || return 1
  contains "$(node_why_not nonsense)" "not a Node version" || return 1
  eq "$(node_why_not v24.1.0)" "" || return 1
}
t_partial_versions() {
  world
  # Shorter than x.y.z must not shift a number into the wrong place.
  eq "$(_nv_minor 22)" 0 && eq "$(_nv_patch 22)" 0 && eq "$(_nv_minor 22.12)" 12 && eq "$(_nv_patch 22.12)" 0 \
    && eq "$(_nv_patch v22.12.3)" 3 && eq "$(_nv_minor v22.12.3-rc1)" 12 || return 1
  ! node_version_ok 22 || { echo "a bare 22 has no minor, so it is 22.0"; return 1; }
}
t_compare() {
  world
  [ "$(_nv_cmp v26.10.0 v26.9.9)" -gt 0 ] && [ "$(_nv_cmp v22.12.0 v22.12.0)" -eq 0 ] && [ "$(_nv_cmp v22.9.0 v22.12.0)" -lt 0 ] \
    && [ "$(_nv_cmp v24.0.0 v22.99.99)" -gt 0 ] && [ "$(_nv_cmp v22.12.1 v22.12.0)" -gt 0 ]
}
t_preferred_major() {
  world
  eq "$(node_preferred_major)" 22 || return 1                       # no file
  printf '22\n' > "$REPO_ROOT/.nvmrc";       eq "$(node_preferred_major)" 22 || return 1
  printf 'v24.1.0\n' > "$REPO_ROOT/.nvmrc";  eq "$(node_preferred_major)" 24 || return 1
  printf '22.12\n' > "$REPO_ROOT/.nvmrc";    eq "$(node_preferred_major)" 22 || return 1
  printf 'lts/*\n' > "$REPO_ROOT/.nvmrc";    eq "$(node_preferred_major)" 22 || return 1   # not a number: the default
  printf '\n' > "$REPO_ROOT/.nvmrc";         eq "$(node_preferred_major)" 22 || return 1
}
t_repo_nvmrc_is_in_range() {
  world
  # The file the repo really ships must name a version its own range accepts.
  REPO_ROOT="$REPO"; m="$(node_preferred_major)"
  node_version_ok "v$m.99.0" || { echo ".nvmrc names $m, outside the range"; return 1; }
}

# ── the platform and the checksum ─────────────────────────────────────────────

t_platform() {
  world
  uname() { case "$1" in -s) echo "$FAKE_OS" ;; -m) echo "$FAKE_ARCH" ;; esac; }
  FAKE_OS=Darwin FAKE_ARCH=arm64;   eq "$(node_platform)" darwin-arm64 || return 1
  FAKE_OS=Darwin FAKE_ARCH=x86_64;  eq "$(node_platform)" darwin-x64 || return 1
  FAKE_OS=Linux  FAKE_ARCH=x86_64;  eq "$(node_platform)" linux-x64 || return 1
  FAKE_OS=Linux  FAKE_ARCH=aarch64; eq "$(node_platform)" linux-arm64 || return 1   # a Jetson
  FAKE_OS=Linux  FAKE_ARCH=arm64;   eq "$(node_platform)" linux-arm64 || return 1
  FAKE_OS=Linux  FAKE_ARCH=riscv64; ! node_platform >/dev/null || { echo "riscv64 should be unsupported"; return 1; }
  FAKE_OS=FreeBSD FAKE_ARCH=amd64;  ! node_platform >/dev/null || { echo "FreeBSD should be unsupported"; return 1; }
}
t_sha256() {
  world
  printf 'abc' > "$W/f"
  eq "$(node_sha256 "$W/f")" ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
}

# ── a fake nodejs.org ─────────────────────────────────────────────────────────

# fixture <major> <version> <platform>: dist/latest-v<major>.x/ with a listing and a tarball whose
# bin/node reports <version>. Sets DIST, FILE, SHA.
fixture() {
  local major="$1" ver="$2" platform="$3" dir
  FIX_PLATFORM="$platform"
  DIST="$W/dist"; dir="$DIST/latest-v${major}.x"; mkdir -p "$dir" "$W/pack/node-v$ver-$platform/bin"
  FILE="node-v$ver-$platform.tar.gz"
  printf '#!/bin/sh\nif [ "$1" = "--version" ]; then echo v%s; fi\n' "$ver" > "$W/pack/node-v$ver-$platform/bin/node"
  chmod +x "$W/pack/node-v$ver-$platform/bin/node"
  ( cd "$W/pack" && tar -czf "$dir/$FILE" "node-v$ver-$platform" )
  SHA="$(node_sha256 "$dir/$FILE")"
  {
    printf '%s  node-v%s-aix-ppc64.tar.gz\n' "0000000000000000000000000000000000000000000000000000000000000000" "$ver"
    printf '%s  %s\n' "$SHA" "$FILE"
    printf '%s  node-v%s-win-x64.zip\n' "1111111111111111111111111111111111111111111111111111111111111111" "$ver"
  } > "$dir/SHASUMS256.txt"
  export GIAP_NODE_DIST_URL="file://$DIST"
  # The fake uname reads a global: a `local` would be gone by the time node_platform calls it.
  uname() {
    case "$1" in
      -s) case "$FIX_PLATFORM" in darwin*) echo Darwin ;; *) echo Linux ;; esac ;;
      -m) case "$FIX_PLATFORM" in
            darwin-arm64) echo arm64 ;; linux-arm64) echo aarch64 ;; *) echo x86_64 ;;
          esac ;;
    esac
  }
}

t_latest_release() {
  world; fixture 22 22.99.0 linux-x64
  eq "$(node_latest_release 22 linux-x64)" "v22.99.0|$SHA|$FILE" || return 1
  node_latest_release 22 darwin-arm64 >/dev/null; eq "$?" 2 || { echo "a platform with no tarball should be 2"; return 1; }
  node_latest_release 99 linux-x64 >/dev/null 2>&1; eq "$?" 1 || { echo "a major with no listing should be 1"; return 1; }
}
t_version_in_listing_is_taken_from_the_file_name() {
  world; fixture 24 24.11.3 linux-arm64
  r="$(node_latest_release 24 linux-arm64)"; eq "${r%%|*}" v24.11.3
}

t_download_installs_verified_node() {
  world; fixture 22 22.99.0 linux-x64
  bin="$(node_download 22)"; rc=$?; eq "$rc" 0 || return 1
  eq "$bin" "$GIAP_NODE_HOME/v22.99.0/bin" || return 1
  eq "$("$bin/node" --version)" v22.99.0 || return 1
  eq "$("$GIAP_NODE_HOME/current/bin/node" --version)" v22.99.0 || { echo "current does not point at it"; return 1; }
  [ -z "$(names "$GIAP_NODE_HOME" | grep '^\.tmp')" ] || { echo "scratch left behind"; return 1; }
}
t_download_is_idempotent() {
  world; fixture 22 22.99.0 linux-x64
  node_download 22 >/dev/null || return 1
  rm -f "$DIST/latest-v22.x/$FILE"                      # a second run must not need the tarball
  bin="$(node_download 22)"; eq "$?" 0 || return 1
  eq "$("$bin/node" --version)" v22.99.0
}
t_download_refuses_a_bad_checksum() {
  world; fixture 22 22.99.0 linux-x64
  sed "s/$SHA/ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff/" "$DIST/latest-v22.x/SHASUMS256.txt" > "$W/s" && mv "$W/s" "$DIST/latest-v22.x/SHASUMS256.txt"
  out="$(node_download 22 2>&1)"; rc=$?
  eq "$rc" 3 || { echo "a mismatch should be 3, got $rc"; return 1; }
  contains "$out" "CHECKSUM MISMATCH" || return 1
  contains "$out" "Nothing was installed" || return 1
  [ ! -e "$GIAP_NODE_HOME/v22.99.0" ] || { echo "a node that failed its checksum was installed"; return 1; }
  [ -z "$(names "$GIAP_NODE_HOME" | grep -v '^current$')" ] || { echo "left something behind: $(ls -A "$GIAP_NODE_HOME")"; return 1; }
}
t_download_failure_leaves_nothing() {
  world; fixture 22 22.99.0 linux-x64
  rm -f "$DIST/latest-v22.x/$FILE"
  node_download 22 >/dev/null 2>&1; eq "$?" 1 || return 1
  [ -z "$(ls -A "$GIAP_NODE_HOME" 2>/dev/null)" ] || { echo "left something behind: $(ls -A "$GIAP_NODE_HOME")"; return 1; }
}
t_download_unsupported_platform() {
  world; uname() { case "$1" in -s) echo Linux ;; -m) echo riscv64 ;; esac; }
  out="$(node_download 22 2>&1)"; eq "$?" 2 || return 1
  contains "$out" "no official Node build" || return 1
}
t_download_dry_run_touches_nothing() {
  world; fixture 22 22.99.0 linux-x64; DRY_RUN=true
  out="$(node_download 22 2>&1)"; eq "$?" 0 || return 1
  contains "$out" "would" || return 1
  [ ! -e "$GIAP_NODE_HOME" ] || { echo "a dry run created $GIAP_NODE_HOME"; return 1; }
}
t_download_asks_curl_for_https_only() {
  world
  # A curl that records how it was called and fails, so nothing is fetched and the flags can be read.
  printf '#!/bin/sh\necho "$@" >> "%s/curl-args"\nexit 22\n' "$W" > "$W/bin/curl"; chmod +x "$W/bin/curl"
  node_latest_release 22 linux-x64 >/dev/null 2>&1
  args="$(cat "$W/curl-args")"
  contains "$args" " --proto =https --tlsv1.2 " || return 1   # exactly this pair: no other scheme added to it
  lacks "$args" "file" || return 1
  # A local copy of nodejs.org (the tests use one) is the only other scheme, and only for file://.
  : > "$W/curl-args"; GIAP_NODE_DIST_URL="file:///nowhere"
  node_latest_release 22 linux-x64 >/dev/null 2>&1
  contains "$(cat "$W/curl-args")" "--proto =file" || return 1
  # An http:// or ftp:// address is not treated as local: it still gets the https-only flags.
  : > "$W/curl-args"; GIAP_NODE_DIST_URL="http://example.invalid/dist"
  node_latest_release 22 linux-x64 >/dev/null 2>&1
  contains "$(cat "$W/curl-args")" " --proto =https --tlsv1.2 " || return 1
  lacks "$(cat "$W/curl-args")" "--proto =file"
}

# ── finding one that is already here ──────────────────────────────────────────

t_finds_managers_layouts() {
  world
  mk_node "$HOME/.nvm/versions/node/v24.3.0/bin" v24.3.0
  mk_node "$HOME/.local/share/fnm/node-versions/v22.14.0/installation/bin" v22.14.0
  mk_node "$HOME/.volta/tools/image/node/26.0.1/bin" v26.0.1
  mk_node "$HOME/.asdf/installs/nodejs/24.9.0/bin" v24.9.0
  mk_node "$GIAP_NODE_HOME/v22.20.0/bin" v22.20.0
  mk_node "$W/bin" v25.8.2
  c="$(node_candidates)"
  for want in "v24.3.0|$HOME/.nvm/versions/node/v24.3.0/bin|nvm" "v22.14.0|$HOME/.local/share/fnm/node-versions/v22.14.0/installation/bin|fnm" \
              "v26.0.1|$HOME/.volta/tools/image/node/26.0.1/bin|volta" "v24.9.0|$HOME/.asdf/installs/nodejs/24.9.0/bin|asdf" \
              "v22.20.0|$GIAP_NODE_HOME/v22.20.0/bin|download" "v25.8.2|$W/bin|PATH"; do
    contains "$c" "$want" || return 1
  done
}
t_candidate_that_will_not_run_is_skipped() {
  world
  mkdir -p "$HOME/.nvm/versions/node/v22.14.0/bin"; printf '#!/bin/sh\nexit 1\n' > "$HOME/.nvm/versions/node/v22.14.0/bin/node"; chmod +x "$HOME/.nvm/versions/node/v22.14.0/bin/node"
  eq "$(node_candidates)" "" || return 1
}
t_best_is_preferred_major_then_newest() {
  world
  mk_node "$HOME/.nvm/versions/node/v25.1.0/bin" v25.1.0       # outside the range
  mk_node "$HOME/.nvm/versions/node/v22.11.0/bin" v22.11.0     # outside the range
  mk_node "$HOME/.nvm/versions/node/v22.14.0/bin" v22.14.0
  mk_node "$HOME/.nvm/versions/node/v22.15.1/bin" v22.15.1
  mk_node "$HOME/.nvm/versions/node/v24.3.0/bin" v24.3.0
  mk_node "$HOME/.nvm/versions/node/v26.10.0/bin" v26.10.0
  best="$(node_find_ok)"
  eq "${best%%|*}" v22.15.1 || { echo "preferred major 22, newest of it, should win; got $best"; return 1; }
  printf '24\n' > "$REPO_ROOT/.nvmrc"
  best="$(node_find_ok)"; eq "${best%%|*}" v24.3.0 || { echo ".nvmrc 24 should pick 24.3.0; got $best"; return 1; }
  printf '28\n' > "$REPO_ROOT/.nvmrc"
  best="$(node_find_ok)"; eq "${best%%|*}" v26.10.0 || { echo "with no node of the preferred major the newest in range should win; got $best"; return 1; }
}
t_nothing_acceptable_found() {
  world
  mk_node "$HOME/.nvm/versions/node/v25.1.0/bin" v25.1.0
  mk_node "$HOME/.nvm/versions/node/v20.19.0/bin" v20.19.0
  eq "$(node_find_ok)" ""
}

t_use_repo_leaves_a_good_path_alone() {
  world; mk_node "$W/bin" v24.1.0
  out="$(node_use_repo)"; eq "$?" 0 || return 1
  eq "$out" "" || return 1
}
t_use_repo_prefers_the_recorded_node() {
  world; mk_node "$W/bin" v25.8.2
  mk_node "$W/good" v22.14.0; node_record "$W/good" || return 1
  out="$(node_use_repo)"; contains "$out" "recorded" || return 1
  node_use_repo >/dev/null; eq "$(node --version)" v22.14.0
}
t_use_repo_ignores_a_recorded_node_that_is_now_wrong() {
  world; mk_node "$W/bin" v25.8.2
  mk_node "$W/stale" v25.0.0; node_record "$W/stale"
  mk_node "$HOME/.nvm/versions/node/v24.3.0/bin" v24.3.0
  node_use_repo >/dev/null; eq "$(node --version)" v24.3.0
}
t_use_repo_falls_back_to_anything_installed() {
  world; mk_node "$W/bin" v25.8.2
  mk_node "$HOME/.nvm/versions/node/v26.10.0/bin" v26.10.0
  out="$(node_use_repo)"; contains "$out" "v26.10.0" || return 1
  node_use_repo >/dev/null; eq "$(node --version)" v26.10.0
}
t_use_repo_says_so_when_there_is_none() {
  world; mk_node "$W/bin" v25.8.2
  node_use_repo >/dev/null; rc=$?; eq "$rc" 1 || return 1
  eq "$(node --version)" v25.8.2
}
# The doctor tells the recorded node (which the pond also starts on) from any other it found.
t_use_repo_says_where_the_node_came_from() {
  world; mk_node "$W/bin" v24.1.0
  node_use_repo >/dev/null; eq "$NODE_USE_FROM" "" || return 1
  world; mk_node "$W/bin" v25.8.2
  mk_node "$W/good" v22.14.0; node_record "$W/good" || return 1
  node_use_repo >/dev/null; eq "$NODE_USE_FROM" recorded || return 1
  world; mk_node "$W/bin" v25.8.2
  mk_node "$HOME/.nvm/versions/node/v26.10.0/bin" v26.10.0
  node_use_repo >/dev/null; eq "$NODE_USE_FROM" nvm
}
t_record_goes_under_the_node_home() {
  world; node_record "$W/x"; eq "$(cat "$(node_record_file)")" "$W/x" || return 1
  contains "$(node_record_file)" "$GIAP_NODE_HOME"
}
t_how_to_use() {
  world
  eq "$(node_how_to_use v26.10.0 /d nvm)" "nvm use 26.10.0" || return 1
  eq "$(node_how_to_use v22.14.0 /d fnm)" "fnm use 22.14.0" || return 1
  contains "$(node_how_to_use v24.1.0 /d volta)" "volta pin node@24" || return 1
  contains "$(node_how_to_use v24.1.0 /opt/n/bin download)" 'export PATH="/opt/n/bin:$PATH"' || return 1
  contains "$(node_how_to_use v24.1.0 /opt/n/bin PATH)" 'export PATH="/opt/n/bin:$PATH"'
}

# ── the orchestration, with the talking replaced by a log ─────────────────────

stubs() {
  LOG="$W/log"; : > "$LOG"; ASK="$W/ask"; : > "$ASK"; ANSWER=0
  say()  { echo "say: $*" >> "$LOG"; };  ok()   { echo "ok: $*" >> "$LOG"; }
  warn() { echo "warn: $*" >> "$LOG"; }; bad()  { echo "bad: $*" >> "$LOG"; }
  info() { echo "info: $*" >> "$LOG"; }; note() { echo "note: $*" >> "$LOG"; }
  run()  { echo "run: $*" >> "$LOG"; "$@"; }
  confirm() { echo "$1" >> "$ASK"; return "$ANSWER"; }
}
logged() { cat "$LOG"; }

t_ensure_path_is_fine() {
  world; stubs; mk_node "$W/bin" v24.1.0
  node_ensure auto || return 1
  contains "$(logged)" "ok: node v24.1.0 is inside" || return 1
  [ ! -s "$ASK" ] || { echo "asked when nothing was needed"; return 1; }
  [ ! -e "$(node_record_file)" ] || { echo "recorded when PATH was fine"; return 1; }
}
t_ensure_finds_one_already_installed_and_does_not_download() {
  world; stubs; mk_node "$W/bin" v25.8.2
  mk_node "$HOME/.nvm/versions/node/v26.10.0/bin" v26.10.0
  node_ensure auto || return 1
  contains "$(logged)" "nothing to download" || return 1
  contains "$(logged)" "nvm use 26.10.0" || return 1
  [ ! -s "$ASK" ] || { echo "asked before using what is already installed"; return 1; }
  eq "$(cat "$(node_record_file)")" "$HOME/.nvm/versions/node/v26.10.0/bin" || return 1
  contains "$(logged)" "odd-numbered" || return 1
}
t_ensure_dry_run_records_nothing() {
  world; stubs; DRY_RUN=true; mk_node "$W/bin" v25.8.2
  mk_node "$HOME/.nvm/versions/node/v26.10.0/bin" v26.10.0
  node_ensure auto || return 1
  [ ! -e "$(node_record_file)" ] || { echo "a dry run recorded"; return 1; }
  contains "$(logged)" "dry run" || return 1
}
t_ensure_downloads_when_asked_and_nothing_fits() {
  world; stubs; fixture 22 22.99.0 linux-x64; ANSWER=0
  node_ensure auto || return 1
  contains "$(cat "$ASK")" "Download Node 22?" || return 1
  eq "$("$(cat "$(node_record_file)")/node" --version)" v22.99.0 || return 1
  contains "$(logged)" "ready in" || return 1
  contains "$(logged)" "SHA-256" || return 1
}
t_ensure_does_not_download_without_a_yes() {
  world; stubs; fixture 22 22.99.0 linux-x64; ANSWER=1
  node_ensure auto && { echo "should have failed"; return 1; }
  [ ! -e "$GIAP_NODE_HOME" ] || { echo "downloaded after a no: $(ls -A "$GIAP_NODE_HOME")"; return 1; }
  [ ! -e "$(node_record_file)" ] || return 1
}
t_ensure_download_failure_is_reported_and_nothing_installed() {
  world; stubs; fixture 22 22.99.0 linux-x64; rm -f "$DIST/latest-v22.x/$FILE"
  node_ensure download && { echo "should have failed"; return 1; }
  contains "$(logged)" "bad: the download did not complete" || return 1
  [ ! -e "$(node_record_file)" ] || return 1
}
t_ensure_dry_run_download_changes_nothing() {
  world; stubs; fixture 22 22.99.0 linux-x64; DRY_RUN=true
  node_ensure download || return 1
  contains "$(logged)" "dry run: nothing was changed" || return 1
  [ ! -e "$GIAP_NODE_HOME" ] || { echo "a dry run created $GIAP_NODE_HOME"; return 1; }
}
t_ensure_unknown_method() {
  world; stubs
  node_ensure teleport && { echo "should fail"; return 1; }
  contains "$(logged)" "unknown method"
}
t_ensure_manager_that_is_not_there() {
  world; stubs
  node_ensure nvm && { echo "should fail"; return 1; }
  contains "$(logged)" "nvm is not installed"
}
t_ensure_uses_nvm_when_it_is_the_manager() {
  world; stubs; ANSWER=0
  export NVM_DIR="$W/home/.nvm"; mkdir -p "$NVM_DIR"
  # A stand-in nvm.sh whose `nvm install N` creates that version the way nvm would.
  cat > "$NVM_DIR/nvm.sh" <<EOF
nvm() { if [ "\$1" = install ]; then mkdir -p "$NVM_DIR/versions/node/v22.88.0/bin"; printf '#!/bin/sh\nif [ "\$1" = "--version" ]; then echo v22.88.0; fi\n' > "$NVM_DIR/versions/node/v22.88.0/bin/node"; chmod +x "$NVM_DIR/versions/node/v22.88.0/bin/node"; echo "installed \$2" > "$W/nvm-called"; fi; }
EOF
  node_ensure auto || { logged; return 1; }
  contains "$(cat "$W/nvm-called")" "installed 22" || return 1
  contains "$(cat "$ASK")" "Install Node 22 with nvm?" || return 1
  eq "$(cat "$(node_record_file)")" "$NVM_DIR/versions/node/v22.88.0/bin" || return 1
  [ ! -e "$GIAP_NODE_HOME/v22.88.0" ] || { echo "downloaded although nvm was there"; return 1; }
}
t_ensure_uses_fnm_and_volta_commands() {
  world; stubs; ANSWER=0
  for m in fnm volta; do
    rm -rf "$HOME/.local" "$HOME/.volta" "$(node_record_file)"
    cat > "$W/bin/$m" <<EOF
#!/bin/sh
echo "\$@" > "$W/$m-args"
if [ "$m" = fnm ]; then d="$HOME/.local/share/fnm/node-versions/v22.77.0/installation/bin"; else d="$HOME/.volta/tools/image/node/22.77.0/bin"; fi
mkdir -p "\$d"; printf '#!/bin/sh\nif [ "\$1" = "--version" ]; then echo v22.77.0; fi\n' > "\$d/node"; chmod +x "\$d/node"
EOF
    chmod +x "$W/bin/$m"
    node_ensure "$m" || { logged; return 1; }
    if [ "$m" = fnm ]; then eq "$(cat "$W/fnm-args")" "install 22" || return 1; fi
    if [ "$m" = volta ]; then eq "$(cat "$W/volta-args")" "install node@22" || return 1; fi
    rm -f "$W/bin/$m"
  done
}
t_install_deps() {
  world; stubs
  mkdir -p "$REPO_ROOT/pond-desktop" "$REPO_ROOT/extensions/music"
  : > "$REPO_ROOT/pond-desktop/package-lock.json"; : > "$REPO_ROOT/extensions/music/package-lock.json"
  cat > "$W/bin/npm" <<EOF
#!/bin/sh
echo "\$(basename "\$PWD") \$@" >> "$W/npm-log"
EOF
  chmod +x "$W/bin/npm"; mk_node "$W/bin" v24.1.0
  node_install_deps || return 1
  eq "$(cat "$W/npm-log")" "$(printf 'pond-desktop ci\nmusic ci')" || { cat "$W/npm-log"; return 1; }
}
t_install_deps_skips_a_missing_lockfile() {
  world; stubs
  mkdir -p "$REPO_ROOT/pond-desktop"; : > "$REPO_ROOT/pond-desktop/package-lock.json"
  printf '#!/bin/sh\necho "$@" >> "%s/npm-log"\n' "$W" > "$W/bin/npm"; chmod +x "$W/bin/npm"; mk_node "$W/bin" v24.1.0
  node_install_deps || return 1
  contains "$(logged)" "warn: extensions/music has no package-lock.json" || return 1
  eq "$(wc -l < "$W/npm-log" | tr -d ' ')" 1
}
t_install_deps_stops_when_npm_fails() {
  world; stubs
  mkdir -p "$REPO_ROOT/pond-desktop" "$REPO_ROOT/extensions/music"
  : > "$REPO_ROOT/pond-desktop/package-lock.json"; : > "$REPO_ROOT/extensions/music/package-lock.json"
  printf '#!/bin/sh\nexit 1\n' > "$W/bin/npm"; chmod +x "$W/bin/npm"; mk_node "$W/bin" v24.1.0
  node_install_deps && { echo "should fail"; return 1; }
  contains "$(logged)" "bad: npm ci failed in pond-desktop"
}

t_report() {
  world; stubs
  mk_node "$W/bin" v24.1.0
  node_report; eq "$?" 0 || return 1; contains "$(logged)" "ok: node v24.1.0 is inside" || return 1
  stubs; mk_node "$W/bin" v25.8.2
  node_report; eq "$?" 1 || return 1; contains "$(logged)" "warn: node v25.8.2 on PATH is outside" || return 1
  contains "$(logged)" "none installed that would do" || return 1
  stubs; mk_node "$HOME/.nvm/versions/node/v26.10.0/bin" v26.10.0
  node_report; eq "$?" 1 || return 1
  contains "$(logged)" "ok: but node v26.10.0 is installed in nvm and would do" || return 1
  contains "$(logged)" "nvm use 26.10.0"
}
t_use_repo_sets_a_note_a_caller_can_read() {
  world; mk_node "$W/bin" v25.8.2; mk_node "$HOME/.nvm/versions/node/v24.3.0/bin" v24.3.0
  node_use_repo >/dev/null    # run here, not in $(...): the PATH change has to stay
  contains "$NODE_USE_NOTE" "v24.3.0" || return 1
  eq "$(node --version)" v24.3.0 || return 1
  node_use_repo >/dev/null; eq "$NODE_USE_NOTE" "" || { echo "note not cleared when PATH was fine"; return 1; }
}
t_candidates_survive_a_space_in_the_path() {
  world
  HOME="$W/home with space"; mkdir -p "$HOME"
  mk_node "$HOME/Library/Application Support/fnm/node-versions/v22.14.0/installation/bin" v22.14.0
  mk_node "$HOME/.nvm/versions/node/v24.3.0/bin" v24.3.0
  c="$(node_candidates)"
  contains "$c" "v22.14.0|$HOME/Library/Application Support/fnm/node-versions/v22.14.0/installation/bin|fnm" || return 1
  contains "$c" "v24.3.0|$HOME/.nvm/versions/node/v24.3.0/bin|nvm"
}
t_a_binary_that_prints_nothing_is_skipped() {
  world
  mkdir -p "$HOME/.nvm/versions/node/v22.14.0/bin"; printf '#!/bin/sh\nexit 0\n' > "$HOME/.nvm/versions/node/v22.14.0/bin/node"; chmod +x "$HOME/.nvm/versions/node/v22.14.0/bin/node"
  eq "$(node_candidates)" ""
}

# ── the command line: giap.sh is the only front door ──────────────────────────

# giap.sh node … against the real repo, with this test's HOME, PATH and node home.
cli() { ( cd "$REPO" && /bin/bash scripts/giap.sh "$@" 2>&1 ); }
t_cli_help_and_bad_options() {
  world
  contains "$(cli --help)" "giap.sh node" || return 1
  cli node --frobnicate >/dev/null; eq "$?" 2 || return 1
  cli node --major abc >/dev/null;  eq "$?" 2 || return 1
  cli node --method >/dev/null;     eq "$?" 2 || return 1
  cli node --major >/dev/null;      eq "$?" 2 || return 1
}
t_cli_check_exit_codes() {
  world; mk_node "$W/bin" v24.1.0
  out="$(cli node --check)"; eq "$?" 0 || return 1; contains "$out" "inside" || return 1
  mk_node "$W/bin" v25.8.2
  out="$(cli node --check)"; eq "$?" 1 || return 1; contains "$out" "outside" || return 1
  mk_node "$HOME/.nvm/versions/node/v26.10.0/bin" v26.10.0
  out="$(cli node --check)"; eq "$?" 1 || return 1
  contains "$out" "v26.10.0 is installed in nvm and would do" || return 1
  contains "$out" "nvm use 26.10.0" || return 1
}
t_cli_check_changes_nothing() {
  world; mk_node "$W/bin" v25.8.2; mk_node "$HOME/.nvm/versions/node/v26.10.0/bin" v26.10.0
  cli node --check >/dev/null
  [ ! -e "$HOME/.giap" ] || { echo "--check created $HOME/.giap"; return 1; }
}
t_cli_refuses_to_download_without_a_terminal_or_yes() {
  world; mk_node "$W/bin" v25.8.2
  out="$(cli node </dev/null)"; rc=$?
  eq "$rc" 1 || { echo "rc=$rc"; return 1; }
  contains "$out" "refusing to run an action that needs confirmation" || return 1
}
t_cli_uses_an_installed_node_with_no_prompt() {
  world; mk_node "$W/bin" v25.8.2; mk_node "$HOME/.nvm/versions/node/v26.10.0/bin" v26.10.0
  out="$(cli node </dev/null)"; rc=$?
  eq "$rc" 0 || { echo "rc=$rc: $out"; return 1; }
  contains "$out" "nothing to download" || return 1
  eq "$(cat "$GIAP_NODE_HOME/.path")" "$HOME/.nvm/versions/node/v26.10.0/bin"
}
t_cli_downloads_with_yes_and_checks_it() {
  world; fixture 22 22.99.0 linux-x64
  export GIAP_NODE_DIST_URL                                   # giap.sh runs as a child
  # the fake uname is a shell function and a child cannot see it, so put one on PATH
  mkdir -p "$W/fakebin"; printf '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; *) /usr/bin/uname "$@";; esac\n' > "$W/fakebin/uname"; chmod +x "$W/fakebin/uname"
  PATH="$W/fakebin:$PATH"
  out="$(cli node -y)"; rc=$?
  eq "$rc" 0 || { echo "rc=$rc: $out"; return 1; }
  contains "$out" "ready in" || return 1
  eq "$("$(cat "$GIAP_NODE_HOME/.path")/node" --version)" v22.99.0
}
t_cli_dry_run_changes_nothing() {
  world; fixture 22 22.99.0 linux-x64; export GIAP_NODE_DIST_URL
  mkdir -p "$W/fakebin"; printf '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; *) /usr/bin/uname "$@";; esac\n' > "$W/fakebin/uname"; chmod +x "$W/fakebin/uname"
  PATH="$W/fakebin:$PATH"
  out="$(cli node -y --dry-run)"; rc=$?
  eq "$rc" 0 || { echo "rc=$rc: $out"; return 1; }
  [ ! -e "$GIAP_NODE_HOME" ] || { echo "a dry run created $GIAP_NODE_HOME"; return 1; }
}
t_cli_build_ui_tries_to_set_node_up_then_falls_back() {
  world
  # An old node first on the real PATH, and no other anywhere in the scratch home.
  mk_node "$W/bin" v18.20.0; PATH="$W/bin:$ORIG_PATH"
  out="$(cli build-ui </dev/null)"; rc=$?
  eq "$rc" 1 || { echo "rc=$rc"; return 1; }
  contains "$out" "cannot run Vite (needs ^20.19.0 || >=22.12.0)" || return 1
  contains "$out" "No Node inside the range is installed" || return 1     # it tried, and offered the download
  contains "$out" "refusing to run an action that needs confirmation" || return 1   # no terminal, no -y: it did not
  contains "$out" "rsync -az" || return 1                                 # so the old advice is still there
  [ ! -e "$GIAP_NODE_HOME" ] || { echo "downloaded without being asked"; return 1; }
}
t_cli_build_ui_accepts_a_node_that_is_only_ui_capable() {
  world
  # 20.19 is outside the repo's range but inside Vite's: a server (the Jetson) must not be nagged to
  # change it, and building the UI must not try to.
  mk_node "$W/bin" v20.19.0; PATH="$W/bin:$ORIG_PATH"
  out="$(cli build-ui --dry-run </dev/null)"
  lacks "$out" "cannot run Vite" || return 1
  lacks "$out" "No Node inside the range" || return 1
  contains "$out" "npm run build"
}
t_cli_rejects_a_bad_method() {
  world
  out="$(cli node -y --method teleport </dev/null)"; eq "$?" 1 || return 1
  contains "$out" "unknown method"
}

for fn in $(declare -F | awk '{print $3}' | grep '^t_'); do t "${fn#t_}" "$fn"; done

printf '\n%s passed, %s failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
