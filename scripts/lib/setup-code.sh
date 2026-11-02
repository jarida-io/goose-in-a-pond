#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# setup-code.sh — a pond's printed setup card: made at install, fetched at provision, reported by
# doctor. The code itself lives in <data>/setup/setup.json and only `pond-server setup-code` shows
# it (docs/architecture/pond-networking.md, "The setup code").
#
# Sourced, never run. Used by scripts/install.sh, scripts/provision-device.sh (`giap.sh
# provision`) and scripts/giap.sh (doctor). The talking functions use the caller's
# ok/warn/info/note; the tests define them too.
#
# Written for bash 3.2, the only bash on a stock macOS: no `declare -A`, no `mapfile`, no `${x,,}`.
# ─────────────────────────────────────────────────────────────────────────────

# Where provision saves a pond's card on the operator's machine, by the pond's id.
setup_code_card_name() { printf 'pond-%s-setup-card.svg' "$1"; }

# The pond id from setup-code's text ("Pond id: 7f3a"); empty when there is none.
setup_code_id_from() {
  printf '%s\n' "$1" | sed -n 's/^Pond id: \([0-9a-f][0-9a-f][0-9a-f][0-9a-f]\)$/\1/p' | head -1
}

# The pond-server a doctor run asks: a release build, else a debug build, else one on PATH.
setup_code_bin() {
  local root="${1:-.}"
  if [ -x "$root/target/release/pond-server" ]; then printf '%s' "$root/target/release/pond-server"
  elif [ -x "$root/target/debug/pond-server" ]; then printf '%s' "$root/target/debug/pond-server"
  else command -v pond-server 2>/dev/null || true
  fi
}

# Tests stand a fake in for sudo.
SETUP_CODE_SUDO="${SETUP_CODE_SUDO-sudo}"

# setup_code_doctor <pond-server> <data_dir> [service user]: one doctor line. Sets SETUP_CODE_LEVEL
# to ok, info or warn so the caller can count it. Never prints the code: `--status` does not know it.
# shellcheck disable=SC2034  # SETUP_CODE_LEVEL is read by giap.sh's doctor
setup_code_doctor() {
  local bin="$1" data="$2" user="${3:-}" out rc
  SETUP_CODE_LEVEL=info
  if [ -z "$bin" ] || [ ! -x "$bin" ]; then
    info "setup code: no pond-server binary to ask yet"
    return 0
  fi
  if [ -n "$user" ] && [ "$user" != "$(id -un)" ] && [ -n "$SETUP_CODE_SUDO" ]; then
    # The code sits in the service account's private setup/; the doctor never asks for a password.
    out="$($SETUP_CODE_SUDO -n -u "$user" env POND_DATA_DIR="$data" "$bin" setup-code --status 2>&1)"; rc=$?
    case "$rc:$out" in
      *"sudo:"*) info "setup code: kept by $user; doctor cannot read it without sudo"; return 0 ;;
    esac
  else
    out="$(POND_DATA_DIR="$data" "$bin" setup-code --status 2>&1)"; rc=$?
  fi
  case "$rc:$out" in
    0:claimed)
      ok "setup code: claimed (the printed card is no longer accepted)"
      SETUP_CODE_LEVEL=ok ;;
    0:unclaimed)
      info "setup code: unclaimed; the first phone to scan this pond's card sets it up"
      note "print the card: $bin setup-code --card \"$data/setup/card.svg\"" ;;
    0:missing)
      warn "setup code: missing; this pond has no card to be set up with"
      note "make it (the server also makes it at startup): $bin setup-code"
      SETUP_CODE_LEVEL=warn ;;
    *"unrecognized subcommand"*)
      info "setup code: $bin predates setup codes; rebuild it to get one" ;;
    *)
      warn "setup code: unreadable"
      note "$(printf '%s\n' "$out" | head -1)"
      SETUP_CODE_LEVEL=warn ;;
  esac
  return 0
}

# setup_code_install_card <pond-server> <data_dir>: make the code if needed and write its card
# beside it. Prints where the card is and what to do with it; returns non-zero on failure.
setup_code_install_card() {
  local bin="$1" data="$2" card="$2/setup/card.svg"
  if ! POND_DATA_DIR="$data" "$bin" setup-code --card "$card"; then
    warn "could not make this pond's setup card; run $bin setup-code later"
    return 1
  fi
  ok "setup card written to $card"
  note "print it and keep it with the pond: a phone scans it to set this pond up"
  return 0
}

# setup_code_fetch_card <host> <pond-server> <data_dir> [dest]: run the device's setup-code over
# ssh, save its card here (default ./pond-<id>-setup-card.svg, 0600) and print the code. Paths may
# hold $HOME, which the device's shell expands.
setup_code_fetch_card() {
  local host="$1" bin="$2" data="$3" dest="${4:-}" work id
  work="$(mktemp -d "${TMPDIR:-/tmp}/setup-card.XXXXXX")" || return 1
  if ! ssh "$host" "POND_DATA_DIR=\"$data\" \"$bin\" setup-code --card -" \
      > "$work/card.svg" 2> "$work/card.txt"; then
    echo "the Pond did not print its setup card:" >&2
    sed 's/^/  /' "$work/card.txt" >&2
    rm -rf "$work"
    return 1
  fi
  id="$(setup_code_id_from "$(cat "$work/card.txt")")"
  if [ -z "$id" ] || ! grep -q '</svg>' "$work/card.svg"; then
    echo "the Pond's setup-code output was not a card; is its pond-server current?" >&2
    rm -rf "$work"
    return 1
  fi
  [ -n "$dest" ] || dest="./$(setup_code_card_name "$id")"
  # The card carries the code, so it is private here as it is on the Pond.
  if ! (umask 077 && cp "$work/card.svg" "$dest" && chmod 600 "$dest"); then
    echo "could not save the card to $dest" >&2
    rm -rf "$work"
    return 1
  fi
  grep -E '^(Setup code|Pond id|Claimed|Unclaimed)' "$work/card.txt"
  echo "card saved to $dest; print it and keep it with the Pond"
  rm -rf "$work"
  return 0
}
