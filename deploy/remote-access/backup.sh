#!/usr/bin/env bash
# Coordinated, encrypted backup of the remote-access control plane.
#
# Headscale's SQLite database and the enrollment JSON store are one authorization
# state: a copy of either alone can restore inconsistent mappings, so both are taken
# in a single stopped window, in the order the package README requires.
#
# Encryption is to an age recipient whose private key is NOT on this host, so this
# machine writes backups it cannot itself read.
set -euo pipefail
cd "$(dirname "$0")"

RECIPIENTS=runtime/secrets/backup-recipients
OUT=runtime/backups
KEEP="${KEEP:-7}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
ARCHIVE="$OUT/control-plane-$STAMP.tar.age"

[ -s "$RECIPIENTS" ] || { echo "no age recipients in $RECIPIENTS; refusing to write an unencrypted backup" >&2; exit 1; }
mkdir -p "$OUT"; chmod 700 "$OUT"

restarted=0
restart() {
  [ "$restarted" = 1 ] && return 0
  restarted=1
  echo "restarting: headscale -> enrollment -> gateway"
  docker compose up -d headscale  >/dev/null 2>&1 || true
  docker compose up -d enrollment >/dev/null 2>&1 || true
  docker compose up -d gateway    >/dev/null 2>&1 || true
}
trap restart EXIT

echo "stopping: gateway -> enrollment -> headscale"
docker compose stop gateway enrollment >/dev/null 2>&1
docker compose stop headscale          >/dev/null 2>&1

# Check both stores while nothing is writing. A backup of a corrupt store restores a
# corrupt store, and restore time is the worst moment to discover that.
echo "integrity checks:"
python3 - <<'PY'
import json, sqlite3, sys, os
bad = []

db = "runtime/headscale/db.sqlite"
if os.path.exists(db):
    try:
        c = sqlite3.connect("file:%s?mode=ro" % db, uri=True)
        r = c.execute("PRAGMA integrity_check").fetchone()[0]
        n = c.execute("SELECT count(*) FROM sqlite_master WHERE type='table'").fetchone()[0]
        print("  headscale sqlite:  %s (%d tables)" % (r, n))
        if r != "ok": bad.append("headscale integrity_check=%s" % r)
    except Exception as e:
        print("  headscale sqlite:  ERROR %s" % e); bad.append("headscale: %s" % e)
else:
    print("  headscale sqlite:  absent"); bad.append("headscale db missing")

st = "runtime/enrollment/state.json"
if os.path.exists(st):
    try:
        with open(st) as f: d = json.load(f)
        print("  enrollment store:  ok (%d top-level key(s))" % len(d))
    except Exception as e:
        print("  enrollment store:  ERROR %s" % e); bad.append("enrollment: %s" % e)
else:
    print("  enrollment store:  absent"); bad.append("enrollment state.json missing")

if bad:
    sys.stderr.write("aborting, stores not healthy: %s\n" % "; ".join(bad)); sys.exit(1)
PY

echo "archiving and encrypting"
# WAL/SHM are included by copying the whole headscale directory, as the README requires.
tar -C runtime -cf - \
    headscale headscale.yaml enrollment secrets caddy-data caddy-config \
  | age -R "$RECIPIENTS" -o "$ARCHIVE.part"
mv "$ARCHIVE.part" "$ARCHIVE"
chmod 600 "$ARCHIVE"

restart

ls -1t "$OUT"/control-plane-*.tar.age 2>/dev/null | tail -n +$((KEEP+1)) | while read -r old; do
  echo "pruning $(basename "$old")"; rm -f -- "$old"
done

echo "wrote $ARCHIVE ($(stat -c %s "$ARCHIVE") bytes)"
echo "retained $(ls -1 "$OUT"/control-plane-*.tar.age 2>/dev/null | wc -l) archive(s), keep=$KEEP"
