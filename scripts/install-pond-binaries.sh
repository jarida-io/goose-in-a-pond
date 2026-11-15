#!/usr/bin/env bash
# Install the built pond-server and pondnet where the service runs them, out of target/.
#
#   bash scripts/install-pond-binaries.sh            # from target/release
#   bash scripts/install-pond-binaries.sh BUILD_DIR  # from another build directory
#
# Run after every release build, by giap.sh build and jetson/deploy.sh. A user unit that still runs
# a binary inside target/ is pointed at the installed one, once. It restarts nothing: the pond
# running now keeps its binary until the caller restarts it. See scripts/lib/pond-binaries.sh.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/.." && pwd)"
# shellcheck source=lib/pond-binaries.sh
source "$HERE/lib/pond-binaries.sh"

from="${1:-$REPO_ROOT/target/release}"
to="$(pond_bin_dir)"
pond_binaries_install "$from" "$to" || exit 1

user_unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/goose-in-a-pond.service"
system_unit=/etc/systemd/system/goose-in-a-pond.service

if [ -f "$user_unit" ] && pond_unit_runs_from_build "$user_unit"; then
  pond_unit_repoint "$user_unit" "$to" || { echo "could not point $user_unit at $to" >&2; exit 1; }
  systemctl --user daemon-reload || { echo "systemctl --user daemon-reload failed" >&2; exit 1; }
  echo "the service now runs $to/pond-server; the unit as it was is $user_unit.pre-install"
fi

# A system unit belongs to root, and one run as agamemnon needs that account able to reach the
# install directory, so this says what to do rather than doing it.
if [ -f "$system_unit" ] && pond_unit_runs_from_build "$system_unit"; then
  echo "WARNING: $system_unit still runs $(pond_unit_exec_bin "$system_unit")," >&2
  echo "  so cleaning target/ stops the pond from starting. Install to a directory its account can" >&2
  echo "  read (GIAP_BIN_DIR=/usr/local/lib/goose-in-a-pond/bin with sudo), then point ExecStart there." >&2
fi
