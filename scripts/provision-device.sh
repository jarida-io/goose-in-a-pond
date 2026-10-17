#!/usr/bin/env bash
# Provision a Pond: fetch its printed setup card, and give it a device certificate so it joins
# the hosted coordination service without an invite. Run it on the operator's machine, the one
# holding the provisioning key:
#
#   bash scripts/giap.sh provision --host pond --key ~/jarida-provisioning.key
#
# It first has the Pond make its setup code, if it has none, and saves the card here to print.
# Then it creates the device key on the Pond over SSH, signs its public half here, and installs
# the certificate back on the Pond. The provisioning key never leaves this machine and the
# device key never leaves the Pond. See deploy/remote-access/README.md, "Device certificates".
#
# Options:
#   --host HOST         the Pond, as ssh knows it (required)
#   --key FILE          the provisioning key made by `pond-provision keygen` (required)
#   --data-dir DIR      the Pond's data directory (default: ~/.local/share/goose-in-a-pond)
#   --pondnet PATH      the Pond's network helper (default: ~/goose-in-a-pond/target/release/pondnet)
#   --pond-server PATH  the Pond's server (default: ~/goose-in-a-pond/target/release/pond-server)
#   --card FILE         where to save the setup card (default: ./pond-<id>-setup-card.svg)
#
# Written for bash 3.2, which is what /usr/bin/env bash finds on a stock Mac.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/.." && pwd)"
# shellcheck source=lib/setup-code.sh
source "$HERE/lib/setup-code.sh"

host="" key="" data='$HOME/.local/share/goose-in-a-pond' pondnet='$HOME/goose-in-a-pond/target/release/pondnet'
pond_server='$HOME/goose-in-a-pond/target/release/pond-server' card=""
while [ $# -gt 0 ]; do
  case "$1" in
    --host) host="${2:-}"; shift 2 ;;
    --key) key="${2:-}"; shift 2 ;;
    --data-dir) data="${2:-}"; shift 2 ;;
    --pondnet) pondnet="${2:-}"; shift 2 ;;
    --pond-server) pond_server="${2:-}"; shift 2 ;;
    --card) card="${2:-}"; shift 2 ;;
    -h|--help) sed -n '2,21p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
if [ -z "$host" ] || [ -z "$key" ]; then
  echo "--host and --key are required; see --help" >&2
  exit 2
fi
if [ ! -r "$key" ]; then
  echo "cannot read the provisioning key $key" >&2
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
provision="$(command -v pond-provision || true)"
if [ -z "$provision" ]; then
  echo "building pond-provision from native/pondnet ..."
  (cd "$REPO_ROOT/native/pondnet" && go build -o "$work/pond-provision" ./cmd/pond-provision)
  provision="$work/pond-provision"
fi

# First, while nothing on the Pond has changed: a failure here leaves it as it was.
echo "fetching the setup card from $host ..."
if ! setup_code_fetch_card "$host" "$pond_server" "$data" "$card"; then
  echo "the Pond has no setup card yet; is pond-server built there (--pond-server)?" >&2
  exit 1
fi
echo

# $HOME in the defaults is expanded by the Pond's shell, not this one.
device_dir="$data/embedded-network/device"

echo "creating the device key on $host ..."
created="$(ssh "$host" "mkdir -p -m 700 \"$data/embedded-network\" && \"$pondnet\" --device-action create --state \"$device_dir\"")" || {
  echo "the Pond did not create a device key; if it already has one it is never replaced, so a" >&2
  echo "Pond being re-imaged needs $device_dir removed first" >&2
  exit 1
}
public="$(printf '%s' "$created" | python3 -c 'import json,sys; print(json.load(sys.stdin)["devicePublicKey"])')"

echo "signing it with $key ..."
certificate="$("$provision" sign --key "$key" --device-public-key "$public")"

echo "installing the certificate on $host ..."
installed="$(printf '%s' "$certificate" | ssh "$host" "\"$pondnet\" --device-action install --state \"$device_dir\"")"
serial="$(printf '%s' "$installed" | python3 -c 'import json,sys; print(json.load(sys.stdin)["serial"])')"

echo
echo "$host is provisioned; it will join the hosted coordination service without an invite."
echo "device serial: $serial"
echo "Record the serial against this Pond. To revoke it if the Pond is lost:"
echo "  docker compose exec enrollment /pond-enrollment --state /state --revoke-device $serial"
