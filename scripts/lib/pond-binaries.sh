#!/usr/bin/env bash
# The binaries the service runs, installed out of the build folder.
#
# The service used to run target/release/pond-server straight out of the build, and the pond finds
# its remote-access helper beside its own executable (embedded_network.rs: current_exe() with the
# file name pondnet). So `cargo clean`, or any cleanup of target/, deleted both while the pond kept
# running on the inode it had open, and the next boot had nothing to start: systemd failed it with
# 203/EXEC and remote access with "bundled pondnet helper is missing". Installed into their own
# directory, the two binaries survive whatever happens to target/.
#
# Each file is copied beside its destination and renamed over it, which is atomic: a running pond
# keeps the inode it started from, and the next start takes the new one. The one replaced is kept
# as <name>.previous, for a rollback by hand. Nothing else the pond reads moves: extensions/ and
# matter-server/ are found through the checkout the binary was built from (asset_root.rs,
# pond-adapters-matter server_setup.rs), and the LiteRT-LM library under the data directory.
#
# Sourced by giap.sh, scripts/install-pond-binaries.sh and the tests; it defines functions only.

# Where the binaries go. GIAP_BIN_DIR overrides it, for a layout of your own.
pond_bin_dir() {
  printf '%s\n' "${GIAP_BIN_DIR:-$HOME/.local/lib/goose-in-a-pond/bin}"
}

# pond_binaries_install <build dir> <install dir>: pond-server, and pondnet when the build made it.
# Returns 1 with nothing changed when the build has no pond-server or the install directory cannot be
# made; a pondnet that fails to copy leaves the pond-server already installed and returns 1 as well.
pond_binaries_install() {
  local from="$1" to="$2" name
  if [ ! -f "$from/pond-server" ]; then
    printf 'no pond-server in %s; build it first (bash scripts/giap.sh build)\n' "$from" >&2
    return 1
  fi
  mkdir -p "$to" || { printf 'cannot make %s\n' "$to" >&2; return 1; }
  # The helper is refused if anyone but its owner can write it, so neither may be group-writable.
  chmod 0755 "$to" || return 1
  for name in pond-server pondnet; do
    if [ ! -f "$from/$name" ]; then
      # Only pondnet can be absent here: the build without Go makes no helper.
      if [ -f "$to/$name" ]; then
        printf '%s: not in this build, so the installed one stays and may be older than the pond\n' "$name" >&2
      else
        printf '%s: not built, so remote access stays off (bash scripts/giap.sh go, then build again)\n' "$name" >&2
      fi
      continue
    fi
    install -m 0755 "$from/$name" "$to/.$name.new" || { rm -f "$to/.$name.new"; return 1; }
    [ -f "$to/$name" ] && ln -f "$to/$name" "$to/$name.previous"
    mv -f "$to/.$name.new" "$to/$name" || { rm -f "$to/.$name.new"; return 1; }
    printf 'installed %s\n' "$to/$name"
  done
}

# The executable a unit's ExecStart names: its first word, without systemd's -@:+! prefixes.
pond_unit_exec_bin() { # $1 = unit file
  local exec
  exec="$(sed -n 's/^ExecStart=//p' "$1" 2>/dev/null | head -1)"
  exec="${exec%% *}"
  while case "$exec" in [-@:+!]*) true ;; *) false ;; esac; do exec="${exec#?}"; done
  printf '%s\n' "$exec"
}

# Succeeds when the unit runs a pond-server inside a cargo target directory.
pond_unit_runs_from_build() { # $1 = unit file
  case "$(pond_unit_exec_bin "$1")" in
    */target/release/pond-server|*/target/debug/pond-server) return 0 ;;
    *) return 1 ;;
  esac
}

# pond_unit_repoint <unit file> <install dir>: ExecStart runs the installed pond-server, with the
# same arguments. Only the executable changes; the original is kept as <unit>.pre-install.
pond_unit_repoint() {
  local unit="$1" to="$2" old new
  old="$(pond_unit_exec_bin "$unit")"
  [ -n "$old" ] || { printf 'no ExecStart in %s\n' "$unit" >&2; return 1; }
  new="$to/pond-server"
  [ "$old" = "$new" ] && return 0
  cp -p "$unit" "$unit.pre-install" || return 1
  # The path is replaced as a literal, so a checkout path holding sed's specials is safe.
  awk -v old="$old" -v new="$new" '
    !done && index($0, "ExecStart=") == 1 {
      i = index($0, old)
      if (i) { $0 = substr($0, 1, i - 1) new substr($0, i + length(old)); done = 1 }
    }
    { print }
  ' "$unit.pre-install" > "$unit.tmp" && mv -f "$unit.tmp" "$unit"
}
