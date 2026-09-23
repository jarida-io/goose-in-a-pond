#!/usr/bin/env bash
# Rotate the Headscale administration key the enrollment service authenticates with.
# The key is read once at startup, so the order is: mint, install, recreate, verify,
# and only then expire the previous key. Never echoes the key value.
set -euo pipefail
cd "$(dirname "$0")"

EXPIRY="${1:-90d}"
SECRET=runtime/secrets/headscale_admin
strip() { sed 's/\x1b\[[0-9;]*m//g'; }
hs() { docker compose exec -T headscale headscale "$@" < /dev/null; }

old_prefix="$(hs apikeys list | strip | awk -F'|' 'NR>1 && $2 ~ /hskey/ {gsub(/ /,"",$2); sub(/-\*\*\*$/,"",$2); print $2}')"
echo "current key prefix(es): ${old_prefix:-none}"

echo "minting a ${EXPIRY} key"
( umask 077; hs apikeys create --expiration "$EXPIRY" > "$SECRET.new" )
[ -s "$SECRET.new" ] || { echo "mint produced nothing; aborting with the old key intact" >&2; rm -f "$SECRET.new"; exit 1; }

cp -a "$SECRET" "$SECRET.prev"
mv "$SECRET.new" "$SECRET"
chown 65532:65532 "$SECRET"
chmod 400 "$SECRET"

echo "recreating enrollment so it reads the new key"
docker compose up -d --force-recreate enrollment >/dev/null 2>&1

for i in $(seq 1 30); do
  state="$(docker inspect -f '{{.State.Health.Status}}' goose-remote-access-enrollment-1 2>/dev/null || echo unknown)"
  [ "$state" = healthy ] && break
  sleep 2
done

if [ "${state:-unknown}" != healthy ]; then
  echo "enrollment did not become healthy (state=$state); restoring the previous key" >&2
  mv "$SECRET.prev" "$SECRET"
  chown 65532:65532 "$SECRET"; chmod 400 "$SECRET"
  docker compose up -d --force-recreate enrollment >/dev/null 2>&1
  exit 1
fi
echo "enrollment healthy on the new key"

for p in $old_prefix; do
  echo "expiring previous key $p"
  hs apikeys expire --prefix "$p" | strip
done
rm -f "$SECRET.prev"

echo "=== keys after ==="
hs apikeys list | strip
echo "=== policy still installed ==="
hs policy get | strip
