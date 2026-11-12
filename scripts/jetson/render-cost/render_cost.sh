#!/usr/bin/env bash
# What one page render costs on this board, beside the running pond. Each page is rendered
# with the pond's own flags in its own memory-capped user scope, so a runaway browser is
# killed before it can starve the pond; needs no root.
#
# Usage: render_cost.sh <cap-MB> <url>...
# The browser is $POND_RENDER_BROWSER, else the headless shell unpacked under /tmp/chs:
#   curl -o chs.zip https://storage.googleapis.com/chrome-for-testing-public/<version>/linux-arm64/chrome-headless-shell-linux-arm64.zip
# Prints, per page: seconds, the scope's peak memory, and the lowest MemAvailable seen.
set -u
B=${POND_RENDER_BROWSER:-/tmp/chs/chrome-headless-shell-linux-arm64/chrome-headless-shell}
[ -x "$B" ] || { echo "no browser at $B"; exit 2; }
WORK=$(dirname "$(dirname "$B")")
CAP_MB=$1
shift
avail() { awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo; }
pond_rss() { ps -o rss= -C pond-server 2>/dev/null | awk '{s+=$1} END {print int(s/1024)}'; }

echo "before: avail_mb=$(avail) pond_rss_mb=$(pond_rss) swap_free_mb=$(awk '/SwapFree/ {print int($2/1024)}' /proc/meminfo)"
n=0
for url in "$@"; do
  n=$((n + 1))
  unit="pond-render-test-$n-$$"
  profile=$WORK/profile-$n
  rm -rf "$profile"
  start=$(date +%s.%N)
  systemd-run --user --scope --quiet --unit "$unit" \
    -p MemoryMax="${CAP_MB}M" -p MemorySwapMax=0 \
    "$B" --headless --dump-dom --virtual-time-budget=8000 --disable-gpu --no-first-run \
    --no-default-browser-check --disable-extensions --disable-background-networking \
    --disable-sync --disable-default-apps --disable-component-update --mute-audio \
    --hide-scrollbars --incognito --blink-settings=imagesEnabled=false \
    --user-data-dir="$profile" "$url" >"$WORK/out-$n.html" 2>"$WORK/err-$n.txt" &
  pid=$!
  cg=""
  peak=0
  low=$(avail)
  procs=0
  while kill -0 "$pid" 2>/dev/null; do
    if [ -z "$cg" ]; then
      rel=$(systemctl --user show "$unit.scope" -p ControlGroup --value 2>/dev/null)
      [ -n "$rel" ] && [ -r "/sys/fs/cgroup$rel/memory.current" ] && cg="/sys/fs/cgroup$rel"
    fi
    if [ -n "$cg" ]; then
      cur=$(cat "$cg/memory.current" 2>/dev/null || echo 0)
      [ "$cur" -gt "$peak" ] && peak=$cur
      p=$(wc -l <"$cg/cgroup.procs" 2>/dev/null || echo 0)
      [ "$p" -gt "$procs" ] && procs=$p
    fi
    a=$(avail)
    [ "$a" -lt "$low" ] && low=$a
    sleep 0.1
    # The pond gives a render 30 s; so does this.
    if [ "$(echo "$(date +%s.%N) - $start > 30" | bc)" = "1" ]; then
      kill "$pid" 2>/dev/null
      echo "  (stopped at 30 s)"
      break
    fi
  done
  wait "$pid" 2>/dev/null
  code=$?
  end=$(date +%s.%N)
  rm -rf "$profile"
  text=$(sed -e 's/<script[^>]*>.*<\/script>//g' -e 's/<[^>]*>//g' "$WORK/out-$n.html" | tr -d '[:space:]' | wc -c)
  printf '%s\n  exit=%s secs=%.1f peak_mb=%s procs=%s html_kb=%s text_chars=%s lowest_avail_mb=%s\n' \
    "$url" "$code" "$(echo "$end - $start" | bc)" "$((peak / 1048576))" "$procs" \
    "$(($(wc -c <"$WORK/out-$n.html") / 1024))" "$text" "$low"
  grep -i -m1 -E "out of memory|oom|killed" "$WORK/err-$n.txt" | cut -c1-160
done
echo "after: avail_mb=$(avail) pond_rss_mb=$(pond_rss)"
