#!/usr/bin/env bash
# scripts/lib/console.sh — `giap.sh console`: live stats, charts and logs for this machine and a board.
#
# Sourced by giap.sh, after pond-health.sh and console-collect.sh. Bash 3.2: no associative arrays,
# whole-second read timeouts. One background sampler per host writes files under CS_DIR; the screen
# only reads them, so a slow or lost ssh never stalls the keys.

CS_DIR=""; CS_SEL=0; CS_VIEW=dash; CS_MSG=""; CS_MSG_UNTIL=0
CS_LV_FILTER=all; CS_LV_SEARCH=""; CS_LV_BACK=0
CS_HOSTS=(); CS_NAMES=(); CS_PIDS=()
CS_LOADED=""

# Block glyphs when the terminal speaks UTF-8, ASCII otherwise; colour only where it means something.
cs_glyphs() {
  case "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}" in
    *UTF-8*|*utf-8*|*UTF8*|*utf8*)
      CS_BLOCKS="▁ ▂ ▃ ▄ ▅ ▆ ▇ █"; CS_FULL="▓"; CS_EMPTY="░"; CS_RULE="─"; CS_DOT="·" ;;
    *)
      CS_BLOCKS=". : - = + * # @"; CS_FULL="#"; CS_EMPTY="."; CS_RULE="-"; CS_DOT="|" ;;
  esac
  CS_REV=$'\033[7m'; CS_BOLD=$'\033[1m'; CS_DIM=$'\033[2m'; CS_OFF=$'\033[0m'
  if [ -n "${NO_COLOR:-}" ]; then CS_RED=""; CS_YLW=""
  else CS_RED=$'\033[31m'; CS_YLW=$'\033[33m'; fi
}

# ── charts ───────────────────────────────────────────────────────────────────
# Every chart scales to its own maximum; a flat series draws as its lowest step, never as nothing.
CS_AWK_LIB='
function glyphs() { nb = split(blocks, B, " ") }
function spark(s, w, mx,   n, v, i, out, lv) {
  n = split(s, v, " ")
  if (mx <= 0) for (i = 1; i <= n; i++) if (v[i] + 0 > mx) mx = v[i] + 0
  out = ""
  for (i = n - w + 1; i <= n; i++) {
    if (i < 1 || v[i] == "-" || v[i] == "") { out = out " "; continue }
    lv = (mx > 0) ? int((v[i] / mx) * (nb - 1) + 0.5) + 1 : 1
    if (lv < 1) lv = 1; if (lv > nb) lv = nb
    out = out B[lv]
  }
  return out
}
function hbar(p, w,   f, i, out) {
  if (p < 0) p = 0; if (p > 100) p = 100
  f = int(p * w / 100 + 0.5); out = ""
  for (i = 1; i <= w; i++) out = out (i <= f ? full : empty)
  return out
}
function chart(s, rows, w, mx,   n, v, r, i, out, lv, cell) {
  n = split(s, v, " ")
  if (mx <= 0) for (i = 1; i <= n; i++) if (v[i] + 0 > mx) mx = v[i] + 0
  for (r = rows - 1; r >= 0; r--) {
    out = ""
    for (i = n - w + 1; i <= n; i++) {
      if (i < 1 || v[i] == "-" || mx <= 0) { out = out " "; continue }
      lv = int((v[i] / mx) * rows * nb + 0.5); cell = lv - r * nb
      if (cell >= nb) out = out B[nb]
      else if (cell <= 0) out = out (r == 0 && v[i] > 0 ? B[1] : " ")
      else out = out B[cell]
    }
    print out
  }
}'

# cs_spark <width> <max or 0> <values>: one row of eighth blocks, newest on the right; 0 scales to
# the series' own maximum. <values> is one string of numbers split on any whitespace, "-" for none.
cs_spark() { awk -v blocks="$CS_BLOCKS" -v w="$1" -v mx="$2" -v s="$(printf '%s' "$3" | tr '\n' ' ')" "$CS_AWK_LIB"' BEGIN { glyphs(); print spark(s, w, mx) }'; }
# cs_hbar <percent> <width>
cs_hbar() { awk -v full="$CS_FULL" -v empty="$CS_EMPTY" -v p="$1" -v w="$2" "$CS_AWK_LIB"' BEGIN { print hbar(p, w) }'; }
# cs_chart <rows> <width> <max or 0> <values>: a bar chart <rows> tall, eight steps a row.
cs_chart() {
  awk -v blocks="$CS_BLOCKS" -v rows="$1" -v w="$2" -v mx="$3" -v s="$(printf '%s' "$4" | tr '\n' ' ')" "$CS_AWK_LIB"' BEGIN { glyphs(); chart(s, rows, w, mx) }'
}

# ── sampling ─────────────────────────────────────────────────────────────────

# cs_ssh_opts <host>: batch-mode options, plus a shared connection when the host has none configured.
cs_ssh_opts() {
  printf '%s\n' -o BatchMode=yes -o ConnectTimeout=5 -o ServerAliveInterval=5 -o ServerAliveCountMax=2
  if [ "$(ssh -G "$1" 2>/dev/null | awk '/^controlpath /{ print $2 }')" = none ]; then
    printf '%s\n' -o ControlMaster=auto -o "ControlPath=$HOME/.ssh/giap-console-%C" -o ControlPersist=120
  fi
}

# cs_sample <host or ""> <full> <log name> <lines seen>: one collector run, here or on the host.
cs_sample() {
  local host="$1"; shift
  if [ -z "$host" ]; then
    console_collect "$@"
  else
    # shellcheck disable=SC2046 # one option per line, none with spaces
    cat "$CS_LIB/pond-health.sh" "$CS_LIB/console-collect.sh" \
      | ssh $(cs_ssh_opts "$host") "$host" bash -s -- __giap_console_collect "$@"
  fi
}

# cs_store <dir> <full> < sample: files the screen reads. CPU percentages come from the previous
# raw counters; history keeps the last 300 samples and the log the last 3000 lines.
cs_store() {
  local dir="$1" full="$2" out
  out="$(cat)"
  printf '%s\n' "$out" | grep '^log=' | sed 's/^log=//' >> "$dir/log"
  if [ "$(wc -l < "$dir/log")" -gt 4000 ]; then tail -n 3000 "$dir/log" > "$dir/log.tmp" && mv "$dir/log.tmp" "$dir/log"; fi
  if [ "$full" = 1 ]; then
    printf '%s\n' "$out" | grep '^turn=' | sed 's/^turn=//' > "$dir/turns.tmp" && mv "$dir/turns.tmp" "$dir/turns"
    printf '%s\n' "$out" | grep -vE '^(log|turn)=' > "$dir/full.tmp" && mv "$dir/full.tmp" "$dir/full"
  fi
  {
    printf '%s\n' "$out" | grep -vE '^(log|turn)='
    printf '%s\n' "$out" | grep -E '^(cpu_raw|core_raw_[0-9]+)=' | awk -v prev="$dir/raw.prev" '
      BEGIN { while ((getline l < prev) > 0) { split(l, a, "[= ]"); pt[a[1]] = a[2]; pi[a[1]] = a[3] } }
      { split($0, a, "[= ]"); k = a[1]
        if (!(k in pt)) next
        dt = a[2] - pt[k]; di = a[3] - pi[k]
        p = (dt > 0) ? int(100 * (dt - di) / dt + 0.5) : -1
        if (k == "cpu_raw") { if (p >= 0) print "cpu_pct=" p } else if (p >= 0) cores = cores (cores == "" ? "" : " ") p }
      END { if (cores != "") print "core_pcts=" cores }'
  } > "$dir/fast.tmp"
  printf '%s\n' "$out" | grep -E '^(cpu_raw|core_raw_[0-9]+)=' > "$dir/raw.prev" || true
  mv "$dir/fast.tmp" "$dir/fast"
  awk -F= '
    $1 == "t" { t = $2 } $1 == "cpu_pct" { c = $2 } $1 == "gpu_pct" { g = $2 }
    $1 == "mem_total_kb" { mt = $2 } $1 == "mem_used_kb" { mu = $2 } $1 == "power_VDD_IN" { pw = $2 }
    $1 ~ /^temp_/ && $2 + 0 > tmax { tmax = $2 + 0 }
    END { printf "%s %s %s %s %s %s\n", t, (c == "" ? "-" : c), (g == "" ? "-" : g),
            (mt > 0 ? int(100 * mu / mt + 0.5) : "-"), (pw == "" ? "-" : pw), (tmax > 0 ? tmax : "-") }' "$dir/fast" >> "$dir/hist"
  if [ "$(wc -l < "$dir/hist")" -gt 400 ]; then tail -n 300 "$dir/hist" > "$dir/hist.tmp" && mv "$dir/hist.tmp" "$dir/hist"; fi
}

# cs_sampler <index> <host or "">: about once a second until CS_DIR/stop appears; a full sample
# every fifth. An unreachable host is retried every five seconds and says so.
cs_sampler() {
  local dir="$CS_DIR/h$1" host="$2" tick=0 full name="" seen=0 out start
  mkdir -p "$dir"; : > "$dir/log"; : > "$dir/hist"
  while [ ! -f "$CS_DIR/stop" ]; do
    start="$(date +%s)"
    full=0; [ $((tick % 5)) -eq 0 ] && full=1
    if out="$(cs_sample "$host" "$full" "$name" "$seen" 2>"$dir/err")" && [ -n "$out" ]; then
      printf '%s\n' "$out" | cs_store "$dir" "$full"
      name="$(printf '%s\n' "$out" | sed -n 's/^log_name=//p' | head -1)"
      seen="$(printf '%s\n' "$out" | sed -n 's/^log_seen=//p' | head -1)"
      echo "ok $start" > "$dir/state"
      tick=$((tick + 1))
    else
      echo "down $start $(grep -v '^$' "$dir/err" | tail -1)" > "$dir/state"
      tick=0; sleep 4
    fi
    # The Mac's top already waits a second for its sample.
    if [ -n "$host" ] || [ "$(uname -s)" != Darwin ]; then sleep 1; fi
  done
}

cs_start_samplers() {
  local i=0
  while [ "$i" -lt "${#CS_HOSTS[@]}" ]; do
    # Its own stderr goes nowhere: stopping it would print a job notice over the screen.
    cs_sampler "$i" "${CS_HOSTS[$i]}" 2>/dev/null &
    CS_PIDS[$i]=$!
    i=$((i + 1))
  done
}

cs_stop_samplers() {
  local p
  [ -n "$CS_DIR" ] && : > "$CS_DIR/stop"
  for p in "${CS_PIDS[@]:-}"; do
    [ -n "$p" ] || continue
    pkill -P "$p" 2>/dev/null; kill "$p" 2>/dev/null
  done
  wait 2>/dev/null
}

# cs_load <index>: the host's latest sample as S_<key> variables. Keys are checked before printf -v
# names a variable after them, and values are never evaluated: a board's text stays text.
cs_load() {
  local dir="$CS_DIR/h$1" v k val
  for v in $CS_LOADED; do unset "$v"; done
  CS_LOADED=""
  for f in "$dir/full" "$dir/fast"; do
    [ -f "$f" ] || continue
    while IFS='=' read -r k val; do
      case "$k" in ''|*[!A-Za-z0-9_]*) continue ;; esac
      printf -v "S_$k" '%s' "$val"
      CS_LOADED="$CS_LOADED S_$k"
    done < "$f"
  done
}
cs_get() { local n="S_$1"; printf '%s' "${!n:-}"; }

# ── formatting ───────────────────────────────────────────────────────────────

cs_count() { printf '%s\n' "$1" | wc -w | tr -d ' '; }
cs_gib() { awk -v k="${1:-0}" 'BEGIN { printf "%.1f", k / 1048576 }'; }
cs_uptime() { awk -v s="${1:-0}" 'BEGIN { d = int(s / 86400); h = int((s % 86400) / 3600); m = int((s % 3600) / 60)
  if (d > 0) printf "%dd %dh", d, h; else printf "%dh %dm", h, m }'; }
cs_rule() { awk -v r="$CS_RULE" -v w="$1" 'BEGIN { for (i = 0; i < w; i++) printf "%s", r }'; }
# cs_clip <width>: each line on stdin cut to <width> visible columns. Escape sequences and the
# continuation bytes of a UTF-8 glyph take no column; a cut line ends with a reset.
cs_clip() {
  LC_ALL=C awk -v w="$1" 'BEGIN { for (i = 128; i < 192; i++) cont = cont sprintf("%c", i); esc = sprintf("%c", 27) }
  { out = ""; n = 0; i = 1; len = length($0); cut = 0
    while (i <= len) {
      c = substr($0, i, 1)
      if (c == esc) { j = i + 1; while (j <= len && substr($0, j, 1) !~ /[A-Za-z]/) j++
        out = out substr($0, i, j - i + 1); i = j + 1; continue }
      if (index(cont, c) == 0) { if (n >= w) { cut = 1; break }; n++ }
      out = out c; i++
    }
    print out (cut ? esc "[0m" : "")
  }'
}

# cs_pad <text> <width>: cut or pad plain text to exactly <width> columns.
cs_pad() { awk -v s="$1" -v w="$2" 'BEGIN { if (length(s) > w) s = substr(s, 1, w); printf "%-" w "s", s }'; }

# cs_loglines <width>: log lines on stdin as time, level and message, cut to <width>, levels coloured.
cs_loglines() {
  awk -v w="$1" -v red="$CS_RED" -v ylw="$CS_YLW" -v off="$CS_OFF" '{
    line = $0
    if ($1 ~ /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T/) {
      lvl = $2; rest = $0; sub(/^[^ ]+ +[^ ]+ +/, "", rest)
      line = substr($1, 12, 8) " " sprintf("%-5s", lvl) " " rest
    }
    if (length(line) > w) line = substr(line, 1, w)
    if (red != "" && line ~ /^[0-9:]+ ERROR/) line = red line off
    else if (ylw != "" && line ~ /^[0-9:]+ WARN/) line = ylw line off
    print line
  }'
}

# ── screens ──────────────────────────────────────────────────────────────────

# cs_titlebar <cols> <crumb>: the host tabs, the chosen one raised in reverse video.
cs_titlebar() {
  local cols="$1" crumb="$2" i=0 tabs="" plain="" now
  now="$(date +%H:%M:%S)"
  while [ "$i" -lt "${#CS_NAMES[@]}" ]; do
    if [ "$i" -eq "$CS_SEL" ]; then tabs="$tabs ${CS_REV}${CS_BOLD} ${CS_NAMES[$i]} ${CS_OFF}"
    else tabs="$tabs  ${CS_NAMES[$i]} "; fi
    plain="$plain  ${CS_NAMES[$i]} "
    i=$((i + 1))
  done
  local left=" ${CS_BOLD}GIAP console${CS_OFF}${crumb:+ $crumb}"
  local used=$(( 14 + ${#crumb} + (${#crumb} > 0 ? 1 : 0) + ${#plain} + ${#now} + 1 ))
  local gap=$(( cols - used )); [ "$gap" -gt 0 ] || gap=1
  printf '%s%s%*s%s\n' "$left" "$tabs" "$gap" "" "$now"
  printf '%s%s%s\n' "$CS_DIM" "$(cs_rule "$cols")" "$CS_OFF"
}

cs_hoststate() {
  local st age
  st="$(cat "$CS_DIR/h$CS_SEL/state" 2>/dev/null)"
  case "$st" in
    ok*)   age=$(( $(date +%s) - ${st#ok } )); [ "$age" -gt 10 ] && printf 'last sample %ss ago' "$age" ;;
    down*) printf 'no answer: %s' "$(printf '%s' "$st" | cut -d' ' -f3-)" ;;
    "")    printf 'first sample...' ;;
  esac
}

cs_dashboard() {
  local cols="$1" rows="$2" top n
  top="$(cs_dash_top "$cols")"
  printf '%s\n' "$top"
  n=$(( rows - $(printf '%s\n' "$top" | wc -l) - 2 ))
  [ "$n" -gt 0 ] && tail -n "$n" "$CS_DIR/h$CS_SEL/log" 2>/dev/null | cs_loglines $((cols - 1)) | sed 's/^/ /'
  return 0
}

cs_dash_top() {
  local cols="$1" w os name eng mtp kv pond turns dec ttft prompt draft last
  local cpu gpu mem memt memu tmp pwr hist n i core_line
  os="$(cs_get os)"
  name="${CS_NAMES[$CS_SEL]}"
  cs_titlebar "$cols" ""
  if [ ! -s "$CS_DIR/h$CS_SEL/full" ]; then
    printf ' %s%s%s  %s\n' "$CS_BOLD" "$name" "$CS_OFF" "$(cs_hoststate)"
    return 0
  fi

  # Who and what.
  if [ "$os" = linux ]; then
    printf ' %s%s%s %s %s' "$CS_BOLD" "$(cs_get host)" "$CS_OFF" "$CS_DOT" "$(cs_get board)"
    [ -n "$(cs_get l4t)" ] && printf ' %s L4T %s %s nvmap %s' "$CS_DOT" "$(cs_get l4t)" "$CS_DOT" "$(cs_get nvmap)"
    [ -n "$(cs_get power_mode)" ] && printf ' %s %s' "$CS_DOT" "$(cs_get power_mode)"
    printf ' %s up %s\n' "$CS_DOT" "$(cs_uptime "$(cs_get uptime_s)")"
  elif [ "$os" = macos ]; then
    printf ' %s%s%s %s %s %s %s cores\n' "$CS_BOLD" "$(cs_get host)" "$CS_OFF" "$CS_DOT" "$(cs_get board)" "$CS_DOT" "$(cs_get ncpu)"
  else
    printf ' %s%s%s %s\n' "$CS_BOLD" "$name" "$CS_OFF" "$(cs_hoststate)"
  fi

  # The pond: service, health, process.
  pond="$(cs_get svc)"; [ -n "$pond" ] || { [ "$(cs_get health)" = 200 ] && pond=running || pond="not running"; }
  printf ' pond   %s' "$pond"
  [ -n "$(cs_get port)" ] && printf ' %s :%s health %s' "$CS_DOT" "$(cs_get port)" "$(cs_get health | sed 's/^000$/none/')"
  [ -n "$(cs_get pond_pid)" ] && printf ' %s pid %s %s %s GiB %s %s%% cpu' "$CS_DOT" "$(cs_get pond_pid)" "$CS_DOT" "$(cs_gib "$(cs_get pond_rss_kb)")" "$CS_DOT" "$(cs_get pond_cpu)"
  printf '\n'

  # The model: engine, a failed load, MTP and KV.
  eng="$(cs_get engine)"
  if [ -n "$(cs_get engine_failed)" ]; then eng="$eng ${CS_RED}last load FAILED: $(cs_get engine_failed)${CS_OFF}"; fi
  if [ "$(cs_get mtp_turns)" -gt 0 ] 2>/dev/null; then mtp="MTP $(cs_get mtp_rate)% of drafts over $(cs_get mtp_turns) turns"
  elif [ -n "$(cs_get mtp_failed)" ]; then mtp="${CS_YLW}MTP off: $(cs_get mtp_failed)${CS_OFF}"
  elif [ "$(cs_get mtp_logged)" = true ]; then mtp="MTP not drafting"
  else mtp="MTP unreported"; fi
  kv="KV $(cs_get kv_saved) saved $(cs_get kv_loaded) loaded"
  printf ' model  %s %s %s %s %s %s %s\n\n' "$(cs_get chat_model | sed 's/^$/no model chosen/')" "$CS_DOT" "${eng:-unknown}" "$CS_DOT" "$mtp" "$CS_DOT" "$kv"

  # Turns: decode rate as a chart, the rest as sparklines. Shape is the data; nothing is coloured.
  w=$(( cols - 24 )); [ "$w" -gt 10 ] || w=10
  turns="$CS_DIR/h$CS_SEL/turns"
  if [ -s "$turns" ]; then
    dec="$(awk '{ print $3 }' "$turns" | tr '\n' ' ')"
    ttft="$(awk '{ print ($2 == "-" ? "-" : $2 / 1000) }' "$turns" | tr '\n' ' ')"
    prompt="$(awk '{ print $4 }' "$turns" | tr '\n' ' ')"
    draft="$(awk '{ print ($6 == "-" ? "-" : $6 * 100) }' "$turns" | tr '\n' ' ')"
    last="$(tail -1 "$turns")"
    printf ' %sdecode tok/s%s  last %s  max %s  over %s turns\n' "$CS_BOLD" "$CS_OFF" \
      "$(printf '%s' "$last" | awk '{ printf "%.1f", $3 }')" "$(awk 'BEGIN { m = 0 } $3 + 0 > m { m = $3 + 0 } END { printf "%.1f", m }' "$turns")" "$(wc -l < "$turns" | tr -d ' ')"
    cs_chart 3 "$w" 0 "$dec" | sed 's/^/              /'
    printf ' ttft s       %s  last %s\n' "$(cs_spark "$w" 0 "$ttft")" "$(printf '%s' "$last" | awk '{ printf ($2 == "-" ? "-" : "%.2f"), $2 / 1000 }')"
    printf ' prompt tok   %s  last %s\n' "$(cs_spark "$w" 0 "$prompt")" "$(printf '%s' "$last" | awk '{ print $4 }')"
    if printf '%s' "$draft" | grep -q '[0-9]'; then
      printf ' mtp accept   %s  last %s\n' "$(cs_spark "$w" 100 "$draft")" "$(printf '%s' "$last" | awk '{ print ($6 == "-" ? "-" : int($6 * 100 + 0.5) "%") }')"
    fi
  else
    printf ' %s%s%s\n' "$CS_DIM" "no turns in today's log yet" "$CS_OFF"
  fi
  printf '\n'

  # The machine: bars for now, sparklines for the last minutes.
  hist="$CS_DIR/h$CS_SEL/hist"
  # Wide enough to read a trend, narrow enough that the figures after it still fit.
  n=$(( cols - 74 )); [ "$n" -gt 8 ] || n=8
  cpu="$(cs_get cpu_pct)"; gpu="$(cs_get gpu_pct)"
  memt="$(cs_get mem_total_kb)"; memu="$(cs_get mem_used_kb)"
  mem="$(awk -v t="${memt:-0}" -v u="${memu:-0}" 'BEGIN { print (t > 0 ? int(100 * u / t + 0.5) : 0) }')"
  core_line=""
  if [ -n "$(cs_get core_pcts)" ]; then core_line="  cores $(cs_spark "$(cs_count "$(cs_get core_pcts)")" 100 "$(cs_get core_pcts)")"; fi
  printf ' CPU  %s %3s%%  %s%s\n' "$(cs_hbar "${cpu:-0}" 20)" "${cpu:--}" "$(cs_spark "$n" 100 "$(awk '{ print $2 }' "$hist" 2>/dev/null)")" "$core_line"
  if [ -n "$gpu" ]; then
    printf ' GPU  %s %3s%%  %s%s\n' "$(cs_hbar "$gpu" 20)" "$gpu" "$(cs_spark "$n" 100 "$(awk '{ print $3 }' "$hist" 2>/dev/null)")" \
      "$( [ -n "$(cs_get gpu_mhz)" ] && printf '  %s MHz' "$(cs_get gpu_mhz)")"
  fi
  printf ' RAM  %s %3s%%  %s  %s / %s GiB  swap %s / %s GiB\n' "$(cs_hbar "$mem" 20)" "$mem" \
    "$(cs_spark "$n" 100 "$(awk '{ print $4 }' "$hist" 2>/dev/null)")" \
    "$(cs_gib "$memu")" "$(cs_gib "$memt")" "$(cs_gib "$(cs_get swap_used_kb)")" "$(cs_gib "$(cs_get swap_total_kb)")"
  tmp="$(for k in cpu gpu tj; do v="$(cs_get "temp_$k")"; [ -n "$v" ] && printf '%s %s.%sC  ' "$k" "$((v / 10))" "$((v % 10))"; done)"
  pwr="$(cs_get power_VDD_IN)"
  if [ -n "$tmp" ] || [ -n "$pwr" ]; then
    printf ' heat %s' "$tmp"
    [ -n "$pwr" ] && printf ' power %s W %s' "$(awk -v p="$pwr" 'BEGIN { printf "%.1f", p / 1000 }')" "$(cs_spark 16 0 "$(awk '{ print $5 }' "$hist" 2>/dev/null)")"
    [ -n "$(cs_get oc_events)" ] && printf '  over-current %s' "$(cs_get oc_events)"
    printf '\n'
  elif [ "$os" = macos ]; then
    printf ' %sheat and power need sudo on macOS, so they are not shown%s\n' "$CS_DIM" "$CS_OFF"
  fi
  printf '\n'

  printf ' %slog%s %s %s(l to scroll and filter, L for the whole file)%s\n' "$CS_BOLD" "$CS_OFF" "$(cs_rule 3)" "$CS_DIM" "$CS_OFF"
}

# cs_logview_file: the selected host's log through the filter and search, numbered by awk later.
cs_logview_file() {
  local src="$CS_DIR/h$CS_SEL/log"
  case "$CS_LV_FILTER" in
    warn)  grep -aE ' (WARN|ERROR) |"level":"(warn|error)"' "$src" ;;
    trace) grep -aE 'giap::(trace|kv)' "$src" ;;
    turns) grep -aE 'kind="(turn_start|turn_end|inference|tool_result)"' "$src" ;;
    *)     cat "$src" ;;
  esac 2>/dev/null | { if [ -n "$CS_LV_SEARCH" ]; then grep -aiF -- "$CS_LV_SEARCH"; else cat; fi; }
}

cs_logview() {
  local cols="$1" rows="$2" body total first last state
  body=$(( rows - 4 )); [ "$body" -gt 1 ] || body=1
  cs_logview_file > "$CS_DIR/view"
  total="$(wc -l < "$CS_DIR/view" | tr -d ' ')"
  [ "$CS_LV_BACK" -le $((total - body)) ] || CS_LV_BACK=$(( total > body ? total - body : 0 ))
  last=$(( total - CS_LV_BACK )); first=$(( last - body + 1 )); [ "$first" -ge 1 ] || first=1
  if [ "$CS_LV_BACK" -eq 0 ]; then state="FOLLOW"; else state="PAUSED"; fi
  cs_titlebar "$cols" "${CS_DOT} log"
  printf ' filter %s%s  search %s  %s  %s%s%s\n' "$CS_BOLD" "$CS_LV_FILTER" "${CS_LV_SEARCH:-none}${CS_OFF}" \
    "$( [ "$total" -gt 0 ] && printf 'lines %s-%s of %s' "$first" "$last" "$total" || printf 'no lines')" "$CS_REV" "$state" "$CS_OFF"
  [ "$total" -gt 0 ] && sed -n "${first},${last}p" "$CS_DIR/view" | cs_loglines $((cols - 1)) | sed 's/^/ /'
  [ "$total" -gt 0 ] || printf ' %snothing matches%s\n' "$CS_DIM" "$CS_OFF"
}

cs_help() {
  cs_titlebar "$1" "${CS_DOT} keys"
  cat <<EOF

 Everywhere
   q            quit                     tab, 1-9   switch host
   d            doctor on this host      r          restart this host's pond service
   L            the whole log in less    ?          these keys

 Dashboard
   l            open the log view

 Log view
   up, k / down, j     one line          PgUp, b / PgDn, space   one page
   g / G               top / bottom      f                       follow the end again
   a  w  t  u          all, warnings and errors, giap traces, turns
   /                   search (case-insensitive)                 esc    back

 Where the numbers come from
   CPU, RAM and swap: /proc on Linux, top and vm_stat on macOS. GPU: the Jetson's load file,
   or ioreg on a Mac. Heat and power: thermal zones and the INA3221 rails (Jetson only).
   Turns, MTP and KV: the pond's own log, read again every fifth sample.

 Press any key.
EOF
}

cs_footer() {
  local keys
  if [ "$CS_VIEW" = logs ]; then keys=" esc back  arrows/j/k scroll  PgUp/PgDn page  g/G ends  f follow  a w t u filter  / search  L less  q quit"
  else keys=" q quit  tab host  l logs  L less  d doctor  r restart pond  ? keys"; fi
  if [ -n "$CS_MSG" ] && [ "$(date +%s)" -le "$CS_MSG_UNTIL" ]; then keys=" $CS_MSG"; fi
  printf '%s%s%s' "$CS_REV" "$(cs_pad "$keys" "$1")" "$CS_OFF"
}

# cs_frame: one screen, drawn over the last without clearing, so nothing flickers.
cs_frame() {
  local size rows cols body
  size="$(stty size < /dev/tty 2>/dev/null)"; rows="${size% *}"; cols="${size#* }"
  case "$rows$cols" in ''|*[!0-9]*) rows=24; cols=80 ;; esac
  cs_load "$CS_SEL"
  if [ "$cols" -lt 80 ] || [ "$rows" -lt 20 ]; then
    body=" The console needs at least 80 x 20; this terminal is $cols x $rows."
  else
    case "$CS_VIEW" in
      logs) body="$(cs_logview "$cols" "$rows")" ;;
      help) body="$(cs_help "$cols")" ;;
      *)    body="$(cs_dashboard "$cols" "$rows")" ;;
    esac
  fi
  printf '\033[H%s' "$(printf '%s\n' "$body" | head -n $((rows - 1)) | cs_clip "$cols" | sed $'s/$/\033[K/')"
  printf '\033[J\033[%s;1H%s' "$rows" "$(cs_footer "$cols")"
}

# ── keys and actions ─────────────────────────────────────────────────────────

cs_say() { CS_MSG="$1"; CS_MSG_UNTIL=$(( $(date +%s) + ${2:-6} )); }

# Auto-wrap off: a line wider than the terminal is clipped instead of pushing the screen down.
cs_screen_on()  { printf '\033[?1049h\033[?25l\033[?7l'; stty -echo 2>/dev/null; }
cs_screen_off() { stty echo 2>/dev/null; printf '\033[?7h\033[?25h\033[?1049l'; }

# cs_outside <command...>: run on the ordinary screen, then wait for a key before coming back.
cs_outside() {
  cs_screen_off
  "$@"
  printf '\n  Press any key to return to the console. '
  IFS= read -rsn1 _ < /dev/tty
  cs_screen_on
}

cs_remote() { # <host> <command>: a command on the host with a terminal, from its checkout.
  # shellcheck disable=SC2046
  ssh -t $(cs_ssh_opts "$1" | grep -v BatchMode) "$1" "$2"
}

cs_doctor() {
  local host="${CS_HOSTS[$CS_SEL]}"
  if [ -z "$host" ]; then cs_outside bash "$REPO_ROOT/scripts/giap.sh" doctor
  else cs_outside cs_remote "$host" 'cd "$HOME/goose-in-a-pond" 2>/dev/null || { echo "no checkout at ~/goose-in-a-pond"; exit 1; }; bash scripts/giap.sh doctor'; fi
}

cs_less() {
  local host="${CS_HOSTS[$CS_SEL]}" path
  path="$(cs_get log_path)"
  [ -n "$path" ] || { cs_say "no log path yet; wait for the first sample"; return; }
  if [ -z "$host" ]; then cs_screen_off; less +F "$path"; cs_screen_on
  else cs_screen_off; cs_remote "$host" "less +F $(printf '%q' "$path")"; cs_screen_on; fi
}

# cs_restart: asks first, then says what the service is doing afterwards rather than that it ran.
cs_restart() {
  local host="${CS_HOSTS[$CS_SEL]}" k out
  if [ -z "$(cs_get svc)" ]; then cs_say "${CS_NAMES[$CS_SEL]} runs no pond service to restart"; return; fi
  cs_say "Restart the pond on ${CS_NAMES[$CS_SEL]}? It drops any turn in flight. y to confirm" 30
  cs_frame
  IFS= read -rsn1 k < /dev/tty
  [ "$k" = y ] || [ "$k" = Y ] || { cs_say "not restarted"; return; }
  cs_say "restarting the pond on ${CS_NAMES[$CS_SEL]}..." 30; cs_frame
  # A pond run by agamemnon restarts through the login's sudo rule (giap.sh service-account).
  # shellcheck disable=SC2016  # expanded by the pond host's shell
  local cmd='if grep -qs "^User=agamemnon$" /etc/systemd/system/goose-in-a-pond.service; then sudo -n systemctl restart goose-in-a-pond.service && sleep 2 && echo "service $(systemctl is-active goose-in-a-pond.service)"; else systemctl --user restart goose-in-a-pond.service && sleep 2 && echo "service $(systemctl --user is-active goose-in-a-pond.service)"; fi'
  if [ -z "$host" ]; then out="$(bash -c "$cmd" 2>&1)"
  else
    # shellcheck disable=SC2046
    out="$(ssh $(cs_ssh_opts "$host") "$host" "$cmd" 2>&1)"
  fi
  cs_say "restart on ${CS_NAMES[$CS_SEL]}: $(printf '%s' "$out" | tail -1)" 10
}

# cs_key: one key, or a whole escape sequence, within a second; empty when none came.
cs_key() {
  local k rest
  IFS= read -rsn1 -t 1 k < /dev/tty || { printf ''; return; }
  if [ "$k" = $'\033' ]; then
    IFS= read -rsn2 -t 1 rest < /dev/tty || rest=""
    case "$rest" in '[5'|'[6') IFS= read -rsn1 -t 1 _ < /dev/tty ;; esac
    printf 'ESC%s' "$rest"; return
  fi
  printf '%s' "$k"
}

cs_page() { local s; s="$(stty size < /dev/tty 2>/dev/null)"; printf '%s' $(( ${s% *} - 5 )); }

cs_handle() {
  local k="$1" n="${#CS_HOSTS[@]}"
  case "$CS_VIEW:$k" in
    *:q) return 1 ;;
    help:*) [ -n "$k" ] && CS_VIEW=dash ;;
    *:$'\t'|*:ESC'[Z') CS_SEL=$(( (CS_SEL + 1) % n )); CS_LV_BACK=0 ;;
    *:[1-9]) [ "$k" -le "$n" ] && { CS_SEL=$((k - 1)); CS_LV_BACK=0; } ;;
    *:'?') CS_VIEW=help ;;
    *:d) cs_doctor ;;
    *:r) cs_restart ;;
    *:L) cs_less ;;
    dash:l) CS_VIEW=logs; CS_LV_BACK=0 ;;
    logs:ESC) CS_VIEW=dash ;;
    logs:ESC'[A'|logs:k) CS_LV_BACK=$((CS_LV_BACK + 1)) ;;
    logs:ESC'[B'|logs:j) [ "$CS_LV_BACK" -gt 0 ] && CS_LV_BACK=$((CS_LV_BACK - 1)) ;;
    logs:ESC'[5'|logs:b) CS_LV_BACK=$((CS_LV_BACK + $(cs_page))) ;;
    logs:ESC'[6'|logs:' ') CS_LV_BACK=$((CS_LV_BACK - $(cs_page))); [ "$CS_LV_BACK" -ge 0 ] || CS_LV_BACK=0 ;;
    logs:g) CS_LV_BACK=1000000 ;;
    logs:G|logs:f) CS_LV_BACK=0 ;;
    logs:a) CS_LV_FILTER=all; CS_LV_BACK=0 ;;
    logs:w) CS_LV_FILTER=warn; CS_LV_BACK=0 ;;
    logs:t) CS_LV_FILTER=trace; CS_LV_BACK=0 ;;
    logs:u) CS_LV_FILTER=turns; CS_LV_BACK=0 ;;
    logs:/) stty echo 2>/dev/null; printf '\033[%s;1H\033[K%s search: ' "$(stty size < /dev/tty | cut -d' ' -f1)" "$CS_REV"
            IFS= read -r CS_LV_SEARCH < /dev/tty; printf '%s' "$CS_OFF"; stty -echo 2>/dev/null; CS_LV_BACK=0 ;;
  esac
  return 0
}

# ── entry ────────────────────────────────────────────────────────────────────

# cs_hosts [extra host]: this machine, the Jetson the deploy scripts use when ssh knows it, and
# any host named on the command line.
cs_hosts() {
  local alias="${JETSON_HOST:-nano}" here
  here="$(hostname -s 2>/dev/null || hostname)"
  CS_HOSTS=(""); CS_NAMES=("this $( [ "$(uname -s)" = Darwin ] && echo mac || echo host ) ($here)")
  if [ "$(uname -s)" = Darwin ] && grep -qiE "^[[:space:]]*Host[[:space:]]+(.*[[:space:]])?$alias([[:space:]]|$)" "$HOME/.ssh/config" 2>/dev/null; then
    CS_HOSTS+=("$alias"); CS_NAMES+=("$alias")
  fi
  if [ -n "${1:-}" ] && [ "$1" != "$alias" ]; then CS_HOSTS+=("$1"); CS_NAMES+=("$1"); fi
}

# console_main [host] [once]: the console; with once, one sample of <host> (or this machine) drawn
# to stdout without colour.
console_main() {
  CS_LIB="$REPO_ROOT/scripts/lib"
  cs_glyphs
  cs_hosts "${1:-}"
  CS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/giap-console.XXXXXX")"
  if [ "${2:-}" = once ]; then
    [ -z "${1:-}" ] || CS_SEL=$(( ${#CS_HOSTS[@]} - 1 ))
    [ "${CS_HOSTS[$CS_SEL]}" = "${1:-}" ] || CS_SEL=1
    mkdir -p "$CS_DIR/h$CS_SEL"; : > "$CS_DIR/h$CS_SEL/log"; : > "$CS_DIR/h$CS_SEL/hist"
    cs_sample "${1:-}" 1 "" 0 | cs_store "$CS_DIR/h$CS_SEL" 1
    cs_load "$CS_SEL"
    CS_REV=""; CS_BOLD=""; CS_DIM=""; CS_OFF=""; CS_RED=""; CS_YLW=""
    cs_dashboard 100 40
    rm -rf "$CS_DIR"
    return 0
  fi
  if [ ! -t 0 ] || [ ! -t 1 ]; then
    bad "the console needs a terminal; try: bash scripts/giap.sh status"
    rm -rf "$CS_DIR"; return 1
  fi
  trap 'cs_cleanup' EXIT
  trap 'cs_cleanup; exit 130' INT TERM
  # A closed terminal window sends HUP, which would skip the EXIT trap.
  trap 'cs_cleanup; exit 129' HUP
  cs_start_samplers
  cs_screen_on
  cs_say "first samples arrive within a few seconds; ? lists the keys" 5
  local k
  while :; do
    cs_frame
    k="$(cs_key)"
    cs_handle "$k" || break
  done
  # From the menu, the console returns there: its samplers and screen must not outlive it.
  cs_cleanup
  trap - EXIT INT TERM HUP
}

cs_cleanup() {
  [ -n "$CS_DIR" ] || return 0
  cs_stop_samplers
  cs_screen_off
  rm -rf "$CS_DIR"; CS_DIR=""
}
