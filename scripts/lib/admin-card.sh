#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# admin-card.sh — a pond's admin card: the password that unlocks My Pond on a member's phone
# (docs/architecture/pond-administrator.md). `pond-server admin-card` makes it and keeps only a
# verifier; this file seals a recovery copy only root on this machine can print again, and lets
# the service's account switch power modes and restart, nothing else.
#
# Sourced, never run. Used by scripts/giap.sh (`admin-card`, doctor) and scripts/install.sh.
# The talking functions use the caller's ok/warn/info/note; the card itself goes to stdout.
#
# Written for bash 3.2, the only bash on a stock macOS: no `declare -A`, no `mapfile`, no `${x,,}`.
# ─────────────────────────────────────────────────────────────────────────────

# Tests point these at a scratch directory and run without sudo.
ADMIN_CARD_VAULT="${ADMIN_CARD_VAULT:-/var/lib/goose-in-a-pond-admin}"
ADMIN_SUDOERS_FILE="${ADMIN_SUDOERS_FILE:-/etc/sudoers.d/goose-in-a-pond-power}"
ADMIN_SUDO="${ADMIN_SUDO-sudo}"
ADMIN_CARD_OS="${ADMIN_CARD_OS:-$(uname -s)}"
ADMIN_TPM_DEVICE="${ADMIN_TPM_DEVICE:-/dev/tpmrm0}"
ADMIN_KEYCHAIN_SERVICE="goose-in-a-pond-admin-card"

# The password from `pond-server admin-card --json`; empty when there is none.
admin_card_password_from() {
  printf '%s\n' "$1" | sed -n 's/.*"password":"\([0-9A-Z-]*\)".*/\1/p' | head -1
}

# The one sudoers line: exact binaries, a single-digit mode, and a reboot.
admin_card_dropin_text() { # $1 = user, $2 = nvpmodel, $3 = systemctl
  printf '%s\n' \
    "# Written by giap.sh admin-card: My Pond may switch power modes and restart, nothing else." \
    "$1 ALL=(root) NOPASSWD: $2 -m [0-9], $3 reboot"
}

admin_card_has_tpm() {
  [ -c "$ADMIN_TPM_DEVICE" ] && command -v tpm2_create >/dev/null 2>&1 \
    && command -v tpm2_unseal >/dev/null 2>&1
}

# Run as root, in the vault. The primary key comes from the TPM's owner seed, so it is the same
# every time on this board and nowhere else.
admin_card_tpm_sh() {
  # shellcheck disable=SC2016  # expanded by the root shell, not here
  $ADMIN_SUDO env TPM2TOOLS_TCTI="device:$ADMIN_TPM_DEVICE" VAULT="$ADMIN_CARD_VAULT" sh -c '
    set -e; cd "$VAULT"
    tpm2_createprimary -Q -C o -g sha256 -G ecc -c primary.ctx </dev/null
    '"$1"'
    rm -f primary.ctx card.ctx'
}

# admin_card_seal <password>: keep the recovery copy, replacing any older one.
admin_card_seal() {
  local password="$1"
  if [ "$ADMIN_CARD_OS" = Darwin ]; then
    # The Keychain is this login's root of trust on a Mac; -U replaces an older card.
    security add-generic-password -U -s "$ADMIN_KEYCHAIN_SERVICE" -a agamemnon -w "$password" \
      >/dev/null 2>&1 || return 1
    ADMIN_SEALED_IN="your login Keychain"
    return 0
  fi
  $ADMIN_SUDO install -d -m 0700 "$ADMIN_CARD_VAULT" || return 1
  if admin_card_has_tpm; then
    printf '%s' "$password" | admin_card_tpm_sh \
      'tpm2_create -Q -C primary.ctx -g sha256 -u card.pub -r card.priv -i-; rm -f card.txt' \
      || return 1
    ADMIN_SEALED_IN="this board's TPM"
  else
    printf '%s\n' "$password" \
      | $ADMIN_SUDO sh -c "umask 077; cat > '$ADMIN_CARD_VAULT/card.txt'; rm -f '$ADMIN_CARD_VAULT/card.pub' '$ADMIN_CARD_VAULT/card.priv'" \
      || return 1
    ADMIN_SEALED_IN="$ADMIN_CARD_VAULT/card.txt (root only; this machine has no TPM)"
  fi
  return 0
}

# Print the sealed card; non-zero when there is none here.
admin_card_unseal() {
  if [ "$ADMIN_CARD_OS" = Darwin ]; then
    security find-generic-password -s "$ADMIN_KEYCHAIN_SERVICE" -a agamemnon -w 2>/dev/null
    return $?
  fi
  if $ADMIN_SUDO test -f "$ADMIN_CARD_VAULT/card.priv" && admin_card_has_tpm; then
    admin_card_tpm_sh \
      'tpm2_load -Q -C primary.ctx -u card.pub -r card.priv -c card.ctx; tpm2_unseal -c card.ctx; echo'
    return $?
  fi
  $ADMIN_SUDO cat "$ADMIN_CARD_VAULT/card.txt" 2>/dev/null
}

# admin_card_power_dropin <user>: on a Jetson, let the service's account run nvpmodel and a reboot.
admin_card_power_dropin() {
  local user="$1" nv sc tmp
  nv="$(command -v nvpmodel 2>/dev/null)"
  sc="$(command -v systemctl 2>/dev/null)"
  if [ -z "$nv" ] || [ -z "$sc" ]; then
    info "no nvpmodel here: My Pond shows power, and changing modes stays off"
    return 0
  fi
  tmp="$(mktemp "${TMPDIR:-/tmp}/giap-power.XXXXXX")"
  admin_card_dropin_text "$user" "$nv" "$sc" > "$tmp"
  # A sudoers file that does not parse can lock sudo out entirely, so it is checked first.
  if ! $ADMIN_SUDO visudo -cf "$tmp" >/dev/null 2>&1; then
    rm -f "$tmp"
    warn "the power-mode rule did not pass visudo; nothing was installed"
    return 1
  fi
  $ADMIN_SUDO install -m 0440 "$tmp" "$ADMIN_SUDOERS_FILE"; local rc=$?
  rm -f "$tmp"
  [ "$rc" -eq 0 ] && ok "My Pond may switch power modes ($user, $ADMIN_SUDOERS_FILE)"
  return "$rc"
}

# admin_card_doctor <pond-server> <data_dir> [service user]: one doctor line; sets ADMIN_CARD_LEVEL.
# shellcheck disable=SC2034  # ADMIN_CARD_LEVEL is read by giap.sh's doctor
admin_card_doctor() {
  local bin="$1" data="$2" user="${3:-}" out rc
  ADMIN_CARD_LEVEL=info
  if [ -z "$bin" ] || [ ! -x "$bin" ]; then
    info "admin card: no pond-server binary to ask yet"
    return 0
  fi
  if [ -n "$user" ] && [ "$user" != "$(id -un)" ] && [ -n "$ADMIN_SUDO" ]; then
    # The card sits in the service account's private setup/; the doctor never asks for a password.
    if ! out="$($ADMIN_SUDO -n -u "$user" env POND_DATA_DIR="$data" "$bin" admin-card --status 2>&1)"; then
      case "$out" in
        *"sudo:"*)
          info "admin card: kept by $user; check it with: bash scripts/giap.sh admin-card show"
          return 0 ;;
      esac
    fi
    rc=0; [ "$out" = present ] || [ "$out" = missing ] || rc=1
  else
    out="$(POND_DATA_DIR="$data" "$bin" admin-card --status 2>&1)"; rc=$?
  fi
  case "$rc:$out" in
    0:present) ok "admin card: present (My Pond unlocks with it)"; ADMIN_CARD_LEVEL=ok ;;
    0:missing)
      warn "admin card: missing; My Pond cannot be unlocked on this pond"
      note "make it: bash scripts/giap.sh admin-card"
      ADMIN_CARD_LEVEL=warn ;;
    *"unrecognized subcommand"*) info "admin card: $bin predates My Pond; rebuild it" ;;
    *) warn "admin card: unreadable"; note "$(printf '%s\n' "$out" | head -1)"; ADMIN_CARD_LEVEL=warn ;;
  esac
  return 0
}

admin_card_print() { # $1 = password
  printf '\n  Admin card for this Pond (administrator: agamemnon)\n\n      %s\n\n' "$1"
  note "Goose On The Go asks for it in My Pond. Write it on a card and keep it with the setup"
  note "card, where only the household's adults can reach it."
}

# admin_card_run <""|show|rotate> <pond-server> <data_dir> <service user>
admin_card_run() {
  local sub="$1" bin="$2" data="$3" user="$4" out password state
  case "$sub" in
    show)
      if password="$(admin_card_unseal)" && [ -n "$password" ]; then
        admin_card_print "$password"; return 0
      fi
      warn "no sealed admin card on this machine; make a new one: bash scripts/giap.sh admin-card rotate"
      return 1 ;;
    ""|rotate) ;;
    *) warn "admin-card takes show or rotate, not: $sub"; return 2 ;;
  esac
  if [ -z "$bin" ] || [ ! -x "$bin" ]; then
    warn "no pond-server binary yet; build it first (bash scripts/giap.sh build)"
    return 1
  fi
  state="$(POND_DATA_DIR="$data" "$bin" admin-card --status 2>/dev/null)"
  if [ -z "$sub" ] && [ "$state" = present ]; then
    ok "this pond already has an admin card"
    note "print it again: bash scripts/giap.sh admin-card show; replace it: bash scripts/giap.sh admin-card rotate"
    admin_card_power_dropin "$user"
    return 0
  fi
  # The card belongs to the data directory's owner, so it is made as the service's account.
  if [ "$user" = "$(id -un)" ]; then
    out="$(POND_DATA_DIR="$data" "$bin" admin-card --json ${sub:+--rotate})"
  else
    out="$($ADMIN_SUDO -u "$user" env POND_DATA_DIR="$data" "$bin" admin-card --json ${sub:+--rotate})"
  fi
  password="$(admin_card_password_from "$out")"
  if [ -z "$password" ]; then
    warn "pond-server did not make an admin card"
    return 1
  fi
  if admin_card_seal "$password"; then
    ok "admin card made; a recovery copy is sealed in $ADMIN_SEALED_IN"
    note "print it again on this machine: bash scripts/giap.sh admin-card show"
  else
    warn "admin card made, but no recovery copy could be kept: write it down now"
  fi
  [ "$sub" = rotate ] && note "every phone unlocked with the old card is locked again"
  admin_card_print "$password"
  admin_card_power_dropin "$user"
  return 0
}
