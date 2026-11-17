#!/usr/bin/env bash
# Tests for pond-binaries.sh. Run: bash scripts/lib/pond-binaries.test.sh   (also under /bin/bash 3.2)
#
# Files only: each test gets a scratch build directory, install directory and unit file.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# A failed mktemp must stop here: an empty SCRATCH would make the trap below remove the current directory.
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/pond-binaries-test.XXXXXX")" && [ -d "$SCRATCH" ] || { echo "mktemp failed" >&2; exit 1; }
SCRATCH="$(cd "$SCRATCH" && pwd)" || exit 1
trap 'rm -rf "$SCRATCH"' EXIT

# shellcheck source=pond-binaries.sh
source "$HERE/pond-binaries.sh"

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

fresh() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/build" "$W/unit"
  B="$W/build"; I="$W/installed"
}
built() { printf '%s\n' "$2" > "$B/$1"; chmod 0775 "$B/$1"; }
mode()  { if stat -c '%a' "$1" >/dev/null 2>&1; then stat -c '%a' "$1"; else stat -f '%Lp' "$1"; fi; }
unit()  { printf '[Unit]\nDescription=pond\n\n[Service]\nExecStart=%s\nRestart=always\n' "$1" > "$W/unit/u.service"; }

test_installs_both_with_modes_no_one_else_can_write() {
  fresh; built pond-server v1; built pondnet n1
  pond_binaries_install "$B" "$I" >/dev/null || return 1
  eq "$(cat "$I/pond-server")" v1 || return 1
  eq "$(cat "$I/pondnet")" n1 || return 1
  eq "$(mode "$I/pondnet")" 755 || return 1
  eq "$(mode "$I")" 755
}

test_a_second_install_keeps_the_one_it_replaced() {
  fresh; built pond-server v1; built pondnet n1
  pond_binaries_install "$B" "$I" >/dev/null
  built pond-server v2
  pond_binaries_install "$B" "$I" >/dev/null || return 1
  eq "$(cat "$I/pond-server")" v2 || return 1
  eq "$(cat "$I/pond-server.previous")" v1
}

test_a_running_pond_keeps_its_open_binary() {
  fresh; built pond-server v1
  pond_binaries_install "$B" "$I" >/dev/null 2>&1
  exec 3< "$I/pond-server"
  built pond-server v2
  pond_binaries_install "$B" "$I" >/dev/null 2>&1
  # Renamed over, not written into: what was open still reads as it was.
  eq "$(cat <&3)" v1 || return 1
  exec 3<&-
  eq "$(cat "$I/pond-server")" v2
}

test_survives_the_build_being_cleaned() {
  fresh; built pond-server v1; built pondnet n1
  pond_binaries_install "$B" "$I" >/dev/null
  rm -rf "$B"
  [ -f "$I/pond-server" ] && [ -f "$I/pondnet" ]
}

test_no_server_in_the_build_changes_nothing() {
  fresh; built pondnet n1
  if pond_binaries_install "$B" "$I" 2>/dev/null; then echo "should refuse"; return 1; fi
  [ ! -e "$I" ]
}

test_no_helper_built_says_remote_access_stays_off() {
  fresh; built pond-server v1
  local out; out="$(pond_binaries_install "$B" "$I" 2>&1)" || return 1
  contains "$out" "remote access stays off" || return 1
  [ ! -e "$I/pondnet" ]
}

test_no_helper_built_keeps_the_installed_one_and_says_so() {
  fresh; built pond-server v1; built pondnet n1
  pond_binaries_install "$B" "$I" >/dev/null
  rm -f "$B/pondnet"
  local out; out="$(pond_binaries_install "$B" "$I" 2>&1)" || return 1
  contains "$out" "may be older" || return 1
  eq "$(cat "$I/pondnet")" n1
}

test_reads_the_executable_without_systemd_prefixes() {
  fresh; unit "-/home/x/goose-in-a-pond/target/release/pond-server serve --port 8080"
  eq "$(pond_unit_exec_bin "$W/unit/u.service")" /home/x/goose-in-a-pond/target/release/pond-server
}

test_knows_a_unit_that_runs_from_the_build() {
  fresh; unit "/home/x/goose-in-a-pond/target/release/pond-server serve --port 8080"
  pond_unit_runs_from_build "$W/unit/u.service" || return 1
  unit "/home/x/.local/lib/goose-in-a-pond/bin/pond-server serve --port 8080"
  if pond_unit_runs_from_build "$W/unit/u.service"; then echo "installed path read as the build"; return 1; fi
}

test_repoint_changes_only_the_executable() {
  fresh; unit "/home/x/goose-in-a-pond/target/release/pond-server serve --port 8080"
  pond_unit_repoint "$W/unit/u.service" /home/x/.local/lib/goose-in-a-pond/bin || return 1
  eq "$(sed -n 's/^ExecStart=//p' "$W/unit/u.service")" "/home/x/.local/lib/goose-in-a-pond/bin/pond-server serve --port 8080" || return 1
  contains "$(cat "$W/unit/u.service")" "Restart=always" || return 1
  contains "$(cat "$W/unit/u.service.pre-install")" "target/release/pond-server"
}

test_repoint_is_a_no_op_once_done() {
  fresh; unit "/opt/bin/pond-server serve"
  pond_unit_repoint "$W/unit/u.service" /opt/bin || return 1
  [ ! -e "$W/unit/u.service.pre-install" ]
}

test_repoint_takes_a_path_with_sed_specials_literally() {
  fresh; unit "/home/a&b/x.y/target/release/pond-server serve"
  pond_unit_repoint "$W/unit/u.service" "/opt/a&b|c" || return 1
  eq "$(sed -n 's/^ExecStart=//p' "$W/unit/u.service")" "/opt/a&b|c/pond-server serve"
}

checkout() { # a checkout with both asset folders, a node_modules and a .git to leave behind
  C="$W/checkout"
  mkdir -p "$C/extensions/music" "$C/matter-server/src" "$C/matter-server/node_modules/tsx" "$C/matter-server/.git"
  printf '%s\n' "$1" > "$C/matter-server/package.json"
  printf 'm\n' > "$C/extensions/music/server.py"
  R="$W/root"
}

test_assets_are_copied_without_node_modules_or_git() {
  fresh; checkout v1
  pond_assets_install "$C" "$R" >/dev/null || return 1
  eq "$(cat "$R/matter-server/package.json")" v1 || return 1
  [ -f "$R/extensions/music/server.py" ] || { echo "extensions not copied"; return 1; }
  [ ! -e "$R/matter-server/node_modules" ] || { echo "node_modules copied"; return 1; }
  [ ! -e "$R/matter-server/.git" ] || { echo ".git copied"; return 1; }
}

test_assets_survive_the_checkout_being_deleted() {
  fresh; checkout v1
  pond_assets_install "$C" "$R" >/dev/null || return 1
  rm -rf "$C"
  [ -f "$R/matter-server/package.json" ] && [ -d "$R/extensions/music" ]
}

test_a_second_asset_install_replaces_and_drops_removed_files() {
  fresh; checkout v1
  printf 'old\n' > "$C/matter-server/src/gone.ts"
  pond_assets_install "$C" "$R" >/dev/null
  rm "$C/matter-server/src/gone.ts"; printf 'v2\n' > "$C/matter-server/package.json"
  pond_assets_install "$C" "$R" >/dev/null || return 1
  eq "$(cat "$R/matter-server/package.json")" v2 || return 1
  [ ! -e "$R/matter-server/src/gone.ts" ] || { echo "stale file kept"; return 1; }
  eq "$(find "$R" -maxdepth 1 -name '.*' ! -name . | wc -l | tr -d ' ')" 0
}

test_a_checkout_missing_a_folder_changes_nothing() {
  fresh; checkout v1
  pond_assets_install "$C" "$R" >/dev/null
  rm -rf "$C/extensions"; printf 'v2\n' > "$C/matter-server/package.json"
  if pond_assets_install "$C" "$R" 2>/dev/null; then echo "should refuse"; return 1; fi
  eq "$(cat "$R/matter-server/package.json")" v1
}

test_asset_root_is_added_under_service() {
  fresh; unit "/opt/bin/pond-server serve"
  eq "$(pond_unit_set_asset_root "$W/unit/u.service" /opt)" changed || return 1
  eq "$(sed -n '/^\[Service\]/{n;p;}' "$W/unit/u.service")" "Environment=GIAP_ASSET_ROOT=/opt" || return 1
  [ -f "$W/unit/u.service.pre-assets" ]
}

test_asset_root_replaces_one_naming_a_checkout() {
  fresh; unit "/opt/bin/pond-server serve"
  printf 'Environment="GIAP_ASSET_ROOT=/home/x/giap"\n' >> "$W/unit/u.service"
  pond_unit_set_asset_root "$W/unit/u.service" /opt >/dev/null || return 1
  eq "$(grep -c GIAP_ASSET_ROOT "$W/unit/u.service")" 1 || return 1
  grep -qxF "Environment=GIAP_ASSET_ROOT=/opt" "$W/unit/u.service"
}

test_asset_root_is_a_no_op_once_set() {
  fresh; unit "/opt/bin/pond-server serve"
  pond_unit_set_asset_root "$W/unit/u.service" /opt >/dev/null
  rm "$W/unit/u.service.pre-assets"
  eq "$(pond_unit_set_asset_root "$W/unit/u.service" /opt)" "" || return 1
  [ ! -e "$W/unit/u.service.pre-assets" ]
}

test_bin_dir_defaults_and_overrides() {
  HOME=/home/x; unset GIAP_BIN_DIR
  eq "$(pond_bin_dir)" /home/x/.local/lib/goose-in-a-pond/bin || return 1
  GIAP_BIN_DIR=/opt/pond/bin
  eq "$(pond_bin_dir)" /opt/pond/bin
}

for fn in $(declare -F | awk '{print $3}' | grep '^test_'); do t "$fn" "$fn"; done
printf '%d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
