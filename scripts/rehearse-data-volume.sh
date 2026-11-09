#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# rehearse-data-volume.sh — run data-encrypt end to end on scratch data, on the real TPM and LUKS,
# before it touches a pond's own data (docs/architecture/data-volume-encryption.md).
#
#   bash scripts/rehearse-data-volume.sh      # Linux with a TPM; asks for sudo
#
# Everything is named giap-dv-rehearsal and lives under ~/giap-dv-rehearsal, /var/lib and /run;
# the pond's service is never stopped. It encrypts a fake data directory, closes and reopens the
# volume as a boot does, opens it with a test admin card, rotates that card, decrypts it again and
# removes everything it made. Exit 0 only if every check passed.
# ─────────────────────────────────────────────────────────────────────────────
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$HOME/giap-dv-rehearsal"
DATA="$ROOT/data"
export DV_NAME=giap-dv-rehearsal DV_DIR=/var/lib/giap-dv-rehearsal DV_SIZE_MB=256 DV_SERVICE=""
export DV_IMAGE=/var/lib/giap-dv-rehearsal/data.luks DV_STAGING=/run/giap-dv-rehearsal-staging
export DV_HELPER=/usr/local/libexec/giap-dv-rehearsal DV_UNIT=/etc/systemd/system/giap-dv-rehearsal.service

say()  { printf '%s\n' "$*"; }
ok()   { printf '  [ OK ] %s\n' "$*"; }
warn() { printf '  [WARN] %s\n' "$*"; }
bad()  { printf '  [FAIL] %s\n' "$*"; }
info() { printf '  %s\n' "$*"; }
note() { printf '         %s\n' "$*"; }
head1() { printf '\n== %s\n' "$*"; }
confirm() { return 0; }
TEST_CARD="REHEARSAL-CARD-ONE"
admin_card_unseal() { printf '%s\n' "$TEST_CARD"; }
# shellcheck source=lib/data-volume.sh
source "$HERE/lib/data-volume.sh"

FAILS=0
check() { # check <what> <command...>
  local what="$1"; shift
  if "$@" >/dev/null 2>&1; then printf '  PASS  %s\n' "$what"; else printf '  FAIL  %s\n' "$what"; FAILS=$((FAILS + 1)); fi
}
# Called through check, which shellcheck cannot follow.
# shellcheck disable=SC2329
opens_with() { printf '%s' "$1" | sudo cryptsetup open --test-passphrase --key-file - "$DV_IMAGE"; }
# shellcheck disable=SC2329
same_file() { [ "$(cat "$1")" = "$2" ]; }

cleanup() {
  head1 "Remove everything the rehearsal made"
  sudo systemctl disable --now giap-dv-rehearsal.service >/dev/null 2>&1
  [ -x "$DV_HELPER" ] && sudo "$DV_HELPER" close >/dev/null 2>&1
  sudo chattr -i "$DATA" >/dev/null 2>&1
  sudo rm -rf "$DV_DIR" "$DV_HELPER" "$DV_UNIT" "$DV_STAGING" "/run/$DV_NAME.key"
  sudo systemctl daemon-reload
  sudo rm -rf "$ROOT"
  say "  removed $ROOT, $DV_DIR, $DV_HELPER and $DV_UNIT"
}

if [ -e "$ROOT" ] || [ -e "$DV_IMAGE" ]; then
  say "a previous rehearsal was left behind; removing it first"
  cleanup
fi

head1 "Fake pond data in $DATA"
mkdir -p "$DATA/hf_cache/hub" "$DATA/models" "$DATA/engine/data/litert-lm" "$DATA/engine/data/sessions" "$DATA/secrets"
SECRET="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
printf '%s' "$SECRET" > "$DATA/secrets.json"
head -c 1048576 /dev/urandom > "$DATA/pond_system.db"
DB_SUM="$(sha256sum < "$DATA/pond_system.db")"
echo "session" > "$DATA/engine/data/sessions/one.json"
echo "weights" > "$DATA/hf_cache/hub/blob"
echo "model" > "$DATA/models/model.gguf"
echo "litert" > "$DATA/engine/data/litert-lm/model.litertlm"
info "secrets, a 1 MB database, a session, and three kinds of download"

head1 "Encrypt it"
if ! dv_encrypt "$DATA" user "$(id -un)"; then
  bad "data-encrypt failed; see above"; cleanup; exit 1
fi

head1 "Checks"
check "the data directory is the encrypted volume" sh -c "findmnt -n -o SOURCE --target '$DATA' | grep -qx /dev/mapper/$DV_NAME"
check "the secrets read back" same_file "$DATA/secrets.json" "$SECRET"
check "the database is byte for byte the same" sh -c "[ \"\$(sha256sum < '$DATA/pond_system.db')\" = '$DB_SUM' ]"
check "a session read back" grep -q session "$DATA/engine/data/sessions/one.json"
check "downloads moved out and linked back" sh -c "[ -L '$DATA/hf_cache' ] && grep -q weights '$DATA/hf_cache/hub/blob'"
check "the LiteRT models are linked back" sh -c "[ -L '$DATA/engine/data/litert-lm' ] && grep -q litert '$DATA/engine/data/litert-lm/model.litertlm'"
check "the downloads are outside the volume" test -f "$ROOT/data-public/models/model.gguf"
check "the volume opens with the admin card" opens_with "$TEST_CARD"
check "it holds a TPM token" sh -c "sudo cryptsetup luksDump '$DV_IMAGE' | grep -q systemd-tpm2"
# cryptsetup 2.4's luksDump does not decode systemd's token; its JSON says which PCRs it is bound to.
check "the TPM key is bound to no PCRs" sh -c "sudo cryptsetup token export --token-id 0 '$DV_IMAGE' | grep -Eq '\"tpm2-pcrs\": *\[ *\]'"
check "the one-time key is gone (two keyslots)" sh -c "[ \"\$(sudo cryptsetup luksDump '$DV_IMAGE' | grep -cE '^  [0-9]+: luks2')\" = 2 ]"
sudo cryptsetup token export --token-id 0 "$DV_IMAGE" | sed 's/"tpm2-blob":"[^"]*"/"tpm2-blob":"…"/; s/^/    /'
echo

head1 "A boot: close the volume, then let the unit open it with the TPM"
sudo systemctl stop giap-dv-rehearsal.service
check "closed, the data directory is empty" sh -c "[ -z \"\$(ls -A '$DATA')\" ]"
check "closed, nothing can be written into it" sh -c "! touch '$DATA/plain-leak' 2>/dev/null"
sudo systemctl start giap-dv-rehearsal.service
check "the unit opened it with the TPM alone" grep -q session "$DATA/engine/data/sessions/one.json"
check "the secrets survived the reopen" same_file "$DATA/secrets.json" "$SECRET"

head1 "Rotate the admin card"
NEW_CARD="REHEARSAL-CARD-TWO"
check "the recovery key moved to the new card" dv_rekey_card "$TEST_CARD" "$NEW_CARD"
check "the new card opens it" opens_with "$NEW_CARD"
check "the old card no longer does" sh -c "! printf '%s' '$TEST_CARD' | sudo cryptsetup open --test-passphrase --key-file - '$DV_IMAGE'"

head1 "Undo: back to plain files"
if dv_decrypt "$DATA"; then
  check "the data directory is plain again" sh -c "! mountpoint -q '$DATA'"
  check "the secrets are plain and intact" same_file "$DATA/secrets.json" "$SECRET"
  check "the downloads are back in place" sh -c "[ -d '$DATA/hf_cache' ] && [ ! -L '$DATA/hf_cache' ]"
  check "the volume file is gone" sh -c "! sudo test -e '$DV_IMAGE'"
  check "the unit is gone" test ! -e "$DV_UNIT"
else
  bad "data-encrypt undo failed"; FAILS=$((FAILS + 1))
fi

cleanup
say ""
if [ "$FAILS" -eq 0 ]; then say "REHEARSAL PASSED: every check passed"; exit 0; fi
say "REHEARSAL FAILED: $FAILS check(s) failed"; exit 1
