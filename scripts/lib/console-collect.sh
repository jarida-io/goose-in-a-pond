#!/usr/bin/env bash
# scripts/lib/console-collect.sh — one sample of a host for `giap.sh console`, as key=value lines.
#
# Runs on the host it samples: giap.sh sources it for this machine, and pipes it after
# pond-health.sh to `ssh HOST bash -s` for a board, so a board runs this checkout's version rather
# than its own. Reads only; bash 3.2. CPU arrives as raw counters: a percentage needs two samples,
# and only the console keeps the previous one.

CC_SYSTEM_UNIT="${CC_SYSTEM_UNIT:-/etc/systemd/system/goose-in-a-pond.service}"

# console_data_dir: the pond's data directory, as the server picks it (a system unit names its own).
console_data_dir() {
  local unit_dir=""
  [ -f "$CC_SYSTEM_UNIT" ] && unit_dir="$(sed -n 's/^Environment="\{0,1\}POND_DATA_DIR=\([^"]*\)"\{0,1\}$/\1/p' "$CC_SYSTEM_UNIT" | head -1)"
  if [ -n "${POND_DATA_DIR:-}" ]; then printf '%s' "$POND_DATA_DIR"
  elif [ "$(uname -s)" = Darwin ]; then printf '%s' "$HOME/Library/Application Support/goose-in-a-pond"
  elif [ -n "$unit_dir" ]; then printf '%s' "$unit_dir"
  else printf '%s' "${XDG_DATA_HOME:-$HOME/.local/share}/goose-in-a-pond"; fi
}

# console_collect <full> <log name> <log lines seen>: the fast stats and the log lines added since
# <seen>. A full sample (1) adds what changes slowly and what costs a read of the whole log.
console_collect() {
  local full="${1:-1}" seen_name="${2:-}" seen="${3:-0}" d os
  PATH="$PATH:/usr/sbin:/sbin"
  d="$(console_data_dir)"
  echo "t=$(date +%s)"
  if [ "$(uname -s)" = Darwin ]; then os=macos; _cc_mac "$full"; else os=linux; _cc_linux "$full"; fi
  _cc_pond "$d" "$full" "$os"
  _cc_log "$d" "$seen_name" "$seen"
}

_cc_mac() {
  local total
  echo "os=macos"
  if [ "$1" = 1 ]; then
    echo "host=$(hostname -s 2>/dev/null)"
    echo "board=$(sysctl -n machdep.cpu.brand_string 2>/dev/null)"
    echo "ncpu=$(sysctl -n hw.ncpu 2>/dev/null)"
  fi
  # top's first sample counts from boot; the second covers the last second.
  top -l 2 -n 0 -s 1 2>/dev/null | awk '/^CPU usage/ { u = $3; s = $5 }
    END { sub(/%/, "", u); sub(/%/, "", s); if (u != "") printf "cpu_pct=%.0f\n", u + s }'
  total="$(sysctl -n hw.memsize 2>/dev/null)"
  vm_stat 2>/dev/null | awk -v total="${total:-0}" '
    /page size of/ { for (i = 1; i <= NF; i++) if ($i ~ /^[0-9]+$/) ps = $i }
    /^Pages active/ { a = $NF } /^Pages wired down/ { w = $NF } /occupied by compressor/ { c = $NF }
    END { printf "mem_total_kb=%d\nmem_used_kb=%d\n", total / 1024, (a + w + c) * ps / 1024 }'
  sysctl -n vm.swapusage 2>/dev/null | awk '{
    for (i = 1; i <= NF; i++) { if ($i == "total") t = $(i + 2); if ($i == "used") u = $(i + 2) }
    printf "swap_total_kb=%d\nswap_used_kb=%d\n", t * 1024, u * 1024 }'
  ioreg -r -d 1 -w 0 -c IOAccelerator 2>/dev/null \
    | sed -n 's/.*"Device Utilization %"=\([0-9]*\).*/gpu_pct=\1/p' | head -1
}

_cc_linux() {
  local l4t="" f z t n h l v i oc=0 found=false
  echo "os=linux"
  if [ "$1" = 1 ]; then
    echo "host=$(hostname 2>/dev/null)"
    [ -r /proc/device-tree/model ] && echo "board=$(tr -d '\000' < /proc/device-tree/model)"
    if [ -r /etc/nv_tegra_release ]; then
      l4t="$(sed -n 's/.*# R\([0-9]*\).*REVISION: \([0-9.]*\).*/\1.\2/p' /etc/nv_tegra_release | head -1)"
      echo "l4t=$l4t"
      jetson_nvmap_state "$l4t"
      echo "nvmap=$NVMAP_STATE"
    fi
    command -v nvpmodel >/dev/null 2>&1 && echo "power_mode=$(nvpmodel -q 2>/dev/null | sed -n 's/NV Power Mode: //p' | head -1)"
    echo "ncpu=$(grep -c '^cpu[0-9]' /proc/stat)"
  fi
  awk '/^cpu/ { tot = 0; for (i = 2; i <= NF; i++) tot += $i
    k = ($1 == "cpu") ? "cpu_raw" : "core_raw_" substr($1, 4); printf "%s=%d %d\n", k, tot, $5 + $6 }' /proc/stat
  awk '/^MemTotal:/ { t = $2 } /^MemAvailable:/ { a = $2 } /^SwapTotal:/ { st = $2 } /^SwapFree:/ { sf = $2 }
    END { printf "mem_total_kb=%d\nmem_used_kb=%d\nswap_total_kb=%d\nswap_used_kb=%d\n", t, t - a, st, st - sf }' /proc/meminfo
  awk '{ printf "uptime_s=%d\n", $1 }' /proc/uptime
  for f in /sys/devices/platform/bus@0/*.gpu/load /sys/devices/platform/*.gpu/load /sys/devices/platform/gpu.0/load; do
    [ -r "$f" ] && { echo "gpu_pct=$(( $(cat "$f" 2>/dev/null || echo 0) / 10 ))"; break; }
  done
  for f in /sys/devices/platform/bus@0/*.gpu/devfreq/*/cur_freq; do
    [ -r "$f" ] && { echo "gpu_mhz=$(( $(cat "$f" 2>/dev/null || echo 0) / 1000000 ))"; break; }
  done
  # Tenths of a degree; a zone that refuses a read (the Orin's cv* zones do) is left out.
  for z in /sys/class/thermal/thermal_zone*; do
    t="$(cat "$z/temp" 2>/dev/null)" || continue
    case "$t" in ''|*[!0-9-]*) continue ;; esac
    n="$(cat "$z/type" 2>/dev/null)"; n="${n%-thermal}"
    echo "temp_$(printf '%s' "$n" | tr -c 'A-Za-z0-9_\n' '_')=$((t / 100))"
  done
  # INA3221 rails, in milliwatts: millivolts times milliamps.
  for h in /sys/bus/i2c/drivers/ina3221/*/hwmon/hwmon*; do
    for l in "$h"/in*_label; do
      [ -r "$l" ] || continue
      n="${l##*/in}"; n="${n%_label}"
      v="$(cat "$h/in${n}_input" 2>/dev/null)"; i="$(cat "$h/curr${n}_input" 2>/dev/null)"
      [ -n "$v" ] && [ -n "$i" ] || continue
      echo "power_$(tr -c 'A-Za-z0-9_\n' '_' < "$l")=$((v * i / 1000))"
    done
  done
  for f in /sys/devices/platform/soctherm-oc-event/hwmon/hwmon*/oc*_event_cnt; do
    [ -r "$f" ] || continue
    found=true; oc=$((oc + $(cat "$f" 2>/dev/null || echo 0)))
  done
  [ "$found" = true ] && echo "oc_events=$oc"
  return 0
}

_cc_pond() {
  local d="$1" full="$2" os="$3" port="" pid="" log
  [ -r "$d/.runtime_api_port" ] && port="$(cat "$d/.runtime_api_port" 2>/dev/null)"
  echo "port=$port"
  if [ -n "$port" ] && command -v curl >/dev/null 2>&1; then
    echo "health=$(curl -s -o /dev/null -m 1 -w '%{http_code}' "http://127.0.0.1:$port/api/v1/health" 2>/dev/null)"
  fi
  if command -v systemctl >/dev/null 2>&1 && [ -f "$CC_SYSTEM_UNIT" ]; then
    echo "svc=$(systemctl is-active goose-in-a-pond.service 2>/dev/null)"
    pid="$(systemctl show -p MainPID --value goose-in-a-pond.service 2>/dev/null)"
    [ "$pid" = 0 ] && pid=""
  elif command -v systemctl >/dev/null 2>&1 && systemctl --user cat goose-in-a-pond.service >/dev/null 2>&1; then
    echo "svc=$(systemctl --user is-active goose-in-a-pond.service 2>/dev/null)"
    pid="$(systemctl --user show -p MainPID --value goose-in-a-pond.service 2>/dev/null)"
    [ "$pid" = 0 ] && pid=""
  else
    pid="$(pgrep -f 'pond-server serve' 2>/dev/null | head -1)"
  fi
  if [ -n "$pid" ]; then
    echo "pond_pid=$pid"
    ps -o rss= -o %cpu= -p "$pid" 2>/dev/null | awk 'NF == 2 { printf "pond_rss_kb=%d\npond_cpu=%.0f\n", $1, $2 }'
  fi
  log="$(pond_log_today "$d")"
  echo "log_path=$log"
  [ "$full" = 1 ] || return 0
  if command -v sqlite3 >/dev/null 2>&1 && [ -f "$d/pond_system.db" ]; then
    sqlite3 "$d/pond_system.db" "SELECT key || '=' || value FROM settings WHERE key IN ('chat_model','chat_provider','thinking_mode');" 2>/dev/null
  fi
  pond_engine_state "$log" "$os"
  echo "engine=$POND_ENGINE"; echo "engine_failed=$POND_ENGINE_FAILED"
  pond_mtp_state "$log"
  echo "mtp_turns=$POND_MTP_TURNS"; echo "mtp_rate=$POND_MTP_RATE"; echo "mtp_tps=$POND_MTP_TPS"
  echo "mtp_logged=$POND_MTP_LOGGED"; echo "mtp_failed=$POND_MTP_FAILED"
  pond_kv_state "$log"
  echo "kv_saved=$POND_KV_SAVED"; echo "kv_loaded=$POND_KV_LOADED"; echo "kv_not_written=$POND_KV_NOT_WRITTEN"
  # The last turns as `turn=<time> <ttft ms> <decode tok/s> <prompt> <completion> <draft rate>`.
  [ -f "$log" ] && grep -a 'kind="turn_end"' "$log" 2>/dev/null | tail -60 | awk '
    function f(k,   m) { if (match($0, " " k "=[^ ]+")) return substr($0, RSTART + length(k) + 2, RLENGTH - length(k) - 2); return "-" }
    { printf "turn=%s %s %s %s %s %s\n", substr($1, 12, 8), f("ttft_ms"), f("decode_tok_per_sec"),
        f("prompt_tokens"), f("completion_tokens"), f("draft_accept_rate") }'
  return 0
}

# _cc_log <data dir> <log name> <lines seen>: `log=` lines after <seen>, at most the last 400, and
# the new count. A new day's file starts again from its own beginning.
_cc_log() {
  local f name total start esc
  f="$(pond_log_today "$1")"; name="${f##*/}"
  echo "log_name=$name"
  if [ ! -f "$f" ]; then echo "log_seen=0"; return 0; fi
  total="$(wc -l < "$f" | tr -d ' ')"
  start="${3:-0}"
  [ "$name" = "$2" ] && [ "$start" -le "$total" ] || start=0
  [ $((total - start)) -le 400 ] || start=$((total - 400))
  echo "log_seen=$total"
  [ "$total" -gt "$start" ] || return 0
  esc="$(printf '\033')"
  sed -n "$((start + 1)),${total}p" "$f" | tr -d '\000\r' | sed "s/${esc}\[[0-9;]*m//g" | cut -c1-400 | sed 's/^/log=/'
}

# Piped to `bash -s`, the first argument says to sample; sourced, this file only defines.
if [ "${1:-}" = __giap_console_collect ]; then
  shift
  console_collect "$@"
fi
