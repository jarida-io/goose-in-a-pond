#!/usr/bin/env bash
# Refuses (exit 1) to start a measurement that could disturb, or be disturbed by, the serving pond.
# Usage: preflight.sh NEED_MB   (the load's expected footprint; 300 MB must stay free after it)
set -uo pipefail
NEED_MB=${1:-0}
DATA=${POND_DATA_DIR:-$HOME/.local/share/goose-in-a-pond}
ok=1

if pgrep -af '[c]argo build|[c]make --build|[n]vcc ' >/dev/null; then
  echo "preflight: a build is running"; ok=0
fi
running=$(sqlite3 -readonly "$DATA/pond_system.db" \
  "select count(*) from long_tasks where state in ('planning','running','awaiting_approval')" 2>/dev/null || echo "?")
if [ "$running" != 0 ]; then
  echo "preflight: long tasks active: $running"; ok=0
fi
log="$DATA/logs/pond.log.$(date -u +%F)"
last=$(grep -h 'kind="turn_end"' "$log" 2>/dev/null | tail -1 | cut -c1-19)
if [ -n "$last" ]; then
  age=$(( $(date -u +%s) - $(date -u -d "$last" +%s) ))
  echo "preflight: last turn ${age}s ago"
  [ "$age" -lt 180 ] && { echo "preflight: a turn landed in the last 3 minutes"; ok=0; }
fi
avail=$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)
echo "preflight: MemAvailable=${avail} MB, need ${NEED_MB} MB + 300 MB"
[ "$avail" -lt $((NEED_MB + 300)) ] && { echo "preflight: not enough memory"; ok=0; }
systemctl --user is-active --quiet goose-in-a-pond.service || { echo "preflight: pond is not active"; ok=0; }

[ "$ok" = 1 ] && echo "preflight: ok" || exit 1
