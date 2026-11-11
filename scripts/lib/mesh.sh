#!/usr/bin/env bash
# Pond-to-pond mesh, as `giap.sh doctor` reports it. Sourced by giap.sh and mesh.test.sh.
#
# Mesh is a default feature of pond-server, so a current build always has it. An older or
# hand-built binary may not, and then turning Mesh on in the app only says it could not start.
# The binary is asked, not a build stamp: each build carries a log line only it can print.

# The line each build carries; they must match build_mesh_transport in crates/pond-server/src/main.rs.
MESH_MARK_BUILT="mesh listening on a stable, identity-derived port"
MESH_MARK_ABSENT="settings.mesh_enabled is true but this pond-server binary was built without"

# yes, no or unknown: whether the binary at $1 was built with mesh.
mesh_binary_support() {
  local bin="$1"
  if [ ! -f "$bin" ] || ! command -v strings >/dev/null 2>&1; then
    printf 'unknown'; return 0
  fi
  if strings -a "$bin" 2>/dev/null | grep -qF "$MESH_MARK_ABSENT"; then
    printf 'no'
  elif strings -a "$bin" 2>/dev/null | grep -qF "$MESH_MARK_BUILT"; then
    printf 'yes'
  else
    printf 'unknown'
  fi
}

# Doctor lines. mesh_doctor <binary> <mesh_enabled setting: true, false or empty when unknown>.
# Sets MESH_DOCTOR_WARN and MESH_DOCTOR_UNK for the caller's tally.
mesh_doctor() {
  local bin="$1" enabled="$2" support
  MESH_DOCTOR_WARN=0; MESH_DOCTOR_UNK=0
  support="$(mesh_binary_support "$bin")"

  case "$support" in
    yes)
      ok "mesh is built into $bin"
      if [ "$enabled" = "true" ]; then
        note "mesh is on; the server log has its peer id and listening port (grep 'mesh enabled')"
      else
        note "mesh is off; turn it on from the Mesh screen to trust another pond"
      fi
      ;;
    no)
      if [ "$enabled" = "true" ]; then
        warn "mesh is turned on, but $bin was built without it, so it cannot start"
        note "fix: rebuild pond-server; mesh is a default feature (bash scripts/giap.sh build)"
        MESH_DOCTOR_WARN=$((MESH_DOCTOR_WARN + 1))
      else
        note "$bin was built without mesh; a rebuild adds it"
      fi
      ;;
    *)
      if [ "$enabled" = "true" ]; then
        unk "mesh is turned on, but whether $bin includes it cannot be read (no binary, or no 'strings')"
        MESH_DOCTOR_UNK=$((MESH_DOCTOR_UNK + 1))
      else
        note "mesh is off"
      fi
      ;;
  esac
}
