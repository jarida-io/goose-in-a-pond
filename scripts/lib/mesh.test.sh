#!/usr/bin/env bash
# Tests for mesh.sh. Run: bash scripts/lib/mesh.test.sh   (also under /bin/bash 3.2)
#
# No Pond and no real binary: each test writes a fake binary holding the text a real one would.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# A failed mktemp must stop here: an empty SCRATCH would make the trap below remove the current directory.
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/mesh-test.XXXXXX")" && [ -d "$SCRATCH" ] || { echo "mktemp failed" >&2; exit 1; }
SCRATCH="$(cd "$SCRATCH" && pwd)" || exit 1
trap 'rm -rf "$SCRATCH"' EXIT

PASSED=0; FAILED=0
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

ok()   { printf '[OK] %s\n' "$*"; }
warn() { printf '[WARN] %s\n' "$*"; }
unk()  { printf '[??] %s\n' "$*"; }
note() { printf '     %s\n' "$*"; }

# shellcheck source=mesh.sh
source "$HERE/mesh.sh"

binary() { # binary <with|without>: a file holding the marker such a build carries
  local f="$SCRATCH/pond-server.$RANDOM$RANDOM"
  if [ "$1" = with ]; then printf '\0%s\0' "$MESH_MARK_BUILT" > "$f"
  else printf '\0%s the mesh feature\0' "$MESH_MARK_ABSENT" > "$f"; fi
  printf '%s' "$f"
}

test_markers_match_the_server_source() {
  local main="$HERE/../../crates/pond-server/src/main.rs"
  grep -qF "$MESH_MARK_BUILT" "$main" || { echo "MESH_MARK_BUILT not in main.rs"; return 1; }
  grep -qF "$MESH_MARK_ABSENT" "$main" || { echo "MESH_MARK_ABSENT not in main.rs"; return 1; }
}

test_a_mesh_build_is_reported() {
  eq "$(mesh_binary_support "$(binary with)")" yes
}

test_a_build_without_mesh_is_reported() {
  eq "$(mesh_binary_support "$(binary without)")" no
}

test_the_lightning_stub_does_not_read_as_no_mesh() {
  local f="$SCRATCH/pond-server.lightning"
  printf '\0%s\0settings.lightning_enabled is true but this pond-server binary was built without\0' "$MESH_MARK_BUILT" > "$f"
  eq "$(mesh_binary_support "$f")" yes
}

test_a_missing_binary_is_unknown() {
  eq "$(mesh_binary_support "$SCRATCH/none")" unknown
}

test_mesh_on_without_support_warns() {
  local out
  out="$(mesh_doctor "$(binary without)" true; echo "warn=$MESH_DOCTOR_WARN")"
  contains "$out" "[WARN] mesh is turned on"
  contains "$out" "warn=1"
}

test_mesh_off_without_support_is_a_note() {
  local out
  out="$(mesh_doctor "$(binary without)" false; echo "warn=$MESH_DOCTOR_WARN")"
  contains "$out" "a rebuild adds it"
  contains "$out" "warn=0"
}

test_mesh_built_is_ok() {
  local out
  out="$(mesh_doctor "$(binary with)" true; echo "warn=$MESH_DOCTOR_WARN unk=$MESH_DOCTOR_UNK")"
  contains "$out" "[OK] mesh is built into"
  contains "$out" "warn=0 unk=0"
}

test_mesh_on_and_unreadable_is_unknown() {
  local out
  out="$(mesh_doctor "$SCRATCH/none" true; echo "unk=$MESH_DOCTOR_UNK")"
  contains "$out" "[??]"
  contains "$out" "unk=1"
}

for fn in $(declare -F | awk '{print $3}' | grep '^test_'); do t "$fn" "$fn"; done
printf '%s passed, %s failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
