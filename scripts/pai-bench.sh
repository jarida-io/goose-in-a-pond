#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# pai-bench.sh — drive all eight PAI capabilities against a REAL local model and
# report what actually happened.
#
#   scripts/pai-bench.sh                 build if needed, run every probe
#   scripts/pai-bench.sh --no-build      use the existing binary
#   scripts/pai-bench.sh --model NAME    pick the GGUF (default: first found)
#   scripts/pai-bench.sh --slow          include the probes that need real time
#                                        (PAI-7's reviewer needs 15 min idle)
#   scripts/pai-bench.sh --only 1,5,8    run a subset
#   scripts/pai-bench.sh --keep          leave the server up to poke at
#   scripts/pai-bench.sh --json FILE     also write the metrics as JSON
#
# WHAT THIS IS FOR, AND WHAT IT REFUSES TO DO
#
# `live-test.sh` proves the pond starts, routes answer and migrations apply. It
# uses the mock provider on purpose, so it says nothing about whether the
# ASSISTANT works. This drives a real GGUF through every PAI capability and
# reports the numbers that decide whether they are usable on this hardware:
# TTFT, prefill and decode rates, prompt cost and what fraction of it is tool
# schemas, KV prefix reuse, reasoning tokens, and per-capability outcomes.
#
# It reports three verdicts and the distinction is the whole point:
#
#   PASS    the capability was EXERCISED and behaved
#   SKIP    it was not exercised, with the reason -- never counted as success
#   FAIL    it was exercised and did not behave
#
# A gate refusing correctly is a PASS only when the refusal is the thing under
# test. Observing that PAI-7's reviewer skipped a tick is a SKIP, not a PASS,
# because the loop running is not the feature working -- a distinction this
# programme paid for: the reviewer ran correctly on the Orin for 32 seconds and
# produced nothing, and only the yield told anyone.
#
# Runs on macOS and Linux. Uses a scratch POND_DATA_DIR with ONE chat model
# HARD-LINKED in, so it gets real inference and can never touch a real pond.
# Not symlinked -- see the note above `link_model` for the 3.1 GB that cost.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DATA_DIR="${TMPDIR:-/tmp}/pai-bench-$$"
DO_BUILD=1
KEEP=0
SLOW=0
MODEL=""
ONLY=""
JSON_OUT=""

while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) DO_BUILD=0 ;;
    --keep)     KEEP=1 ;;
    --slow)     SLOW=1 ;;
    --model)    MODEL="${2:-}"; shift ;;
    --only)     ONLY="${2:-}"; shift ;;
    --json)     JSON_OUT="${2:-}"; shift ;;
    -h|--help)  sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
  shift
done

SERVER_PID=""
cleanup() {
  if [ -n "$SERVER_PID" ] && [ "$KEEP" -eq 0 ]; then
    kill -9 "$SERVER_PID" 2>/dev/null
  fi
  if [ "$KEEP" -eq 0 ]; then
    rm -rf "$DATA_DIR"
  else
    echo
    echo "Left running: pid $SERVER_PID on port ${PORT_RESOLVED:-?}, data in $DATA_DIR"
  fi
}
trap cleanup EXIT

say() { printf '\n=== %s ===\n' "$1"; }

# ── The real models ──────────────────────────────────────────────────────────
#
# HARD-LINKED, one file, never symlinked to the tree.
#
# This script used to do `ln -s <real models dir> $DATA_DIR/models`, with a
# comment claiming the scratch dir owned everything else so a probe "still
# cannot write to a real pond". That claim was false and it destroyed a real
# pond's model: the scratch server's models directory WAS the real one, so the
# registry downloaded a GGUF into the scratch `hf_cache` and rewrote the REAL
# `models/gguf` entry to point at it. Deleting the scratch dir then took a 3 GB
# model with it, and the household's pond could not load its own chat model.
#
# A hard link cannot do that. It is a second directory entry for the same
# inode: the probe's registry may rewrite or delete ITS entry and the real one
# is untouched, because unlinking one name of a two-named inode frees nothing.
# It costs no disk and no copy time. Only the chat model is linked -- the
# mmproj/vision encoder is deliberately absent, and POND_DISABLE_MODEL_PROVISIONING
# (below) keeps the serve process from fetching one into the scratch pond at boot.
case "$(uname -s)" in
  Darwin) REAL_MODELS="$HOME/Library/Application Support/goose-in-a-pond/models" ;;
  *)      REAL_MODELS="$HOME/.local/share/goose-in-a-pond/models" ;;
esac

if [ ! -d "$REAL_MODELS/gguf" ]; then
  echo "FATAL: no GGUF models at $REAL_MODELS/gguf" >&2
  echo "       This benchmark needs a real model; there is nothing to measure" >&2
  echo "       without one. Download one through the app first." >&2
  exit 1
fi

# Pick a model. Default to THE ONE THIS POND IS CONFIGURED TO USE, read from the
# real settings database, because that is the model whose numbers anybody cares
# about.
#
# The first version of this sorted the directory alphabetically and took the
# head, which picked `gemma-4-E2B-it-assistant-F16` -- an MTP research variant
# whose architecture the shipped llama.cpp cannot load at all. Every turn
# failed with `unknown model architecture: 'gemma4_mtp'`, and the run reported
# two unrelated-looking failures and five skips. A benchmark that picks its own
# subject will eventually pick one nobody runs.
MODEL_SOURCE=""
if [ -n "$MODEL" ]; then
  MODEL_SOURCE="--model"
fi
if [ -z "$MODEL" ]; then
  REAL_DB="$(dirname "$REAL_MODELS")/pond_system.db"
  if [ -f "$REAL_DB" ] && command -v sqlite3 >/dev/null 2>&1; then
    MODEL="$(sqlite3 "$REAL_DB" "SELECT value FROM settings WHERE key='chat_model';" 2>/dev/null | head -1 | tr -d ' \r\n')"
    [ -n "$MODEL" ] && MODEL_SOURCE="this pond's chat_model setting"
  fi
fi
# Fall back to a quantised chat model. Q4_K_M and Q5/Q8 are what ships; F16 and
# anything with an exotic architecture are research artefacts that happen to
# sort first.
if [ -z "$MODEL" ]; then
  # LC_ALL=C so the sort is byte order rather than locale collation. Without it
  # the "first" file differs between machines and even between shells, which is
  # how two runs on one laptop benchmarked two different models an hour apart.
  MODEL="$(ls "$REAL_MODELS/gguf" 2>/dev/null \
    | grep -iE '\.gguf$' \
    | grep -iE 'Q[45689]_' \
    | grep -viE 'mmproj|asr|embed|nemotron|whisper|mtp|draft|assistant' \
    | sed 's/\.gguf$//' \
    | LC_ALL=C sort \
    | head -1)"
  [ -n "$MODEL" ] && MODEL_SOURCE="fallback scan of $REAL_MODELS/gguf"
fi

# A benchmark that cannot say WHY it is measuring this model is one whose
# numbers cannot be compared with last week's. Two runs on this laptop picked
# different models an hour apart and nothing in the output said so.
if [ -z "$MODEL" ]; then
  echo "FATAL: could not pick a model from $REAL_MODELS/gguf" >&2
  ls "$REAL_MODELS/gguf" >&2
  exit 1
fi
# ── Server lifecycle ─────────────────────────────────────────────────────────
#
# Lifted wholesale from live-test.sh, including the reasons. Never assume a
# port; read the one the server published; fail hard when it publishes none; and
# prove the listener is our own child before sending a request to it. That last
# check exists because an earlier version of the sibling script drove a
# pond-server it did not start and overwrote four rows in a real pond.
wait_for_port_file() {
  local dir="$1" label="$2" log="$3" tries="${4:-240}"
  for _ in $(seq 1 "$tries"); do
    [ -s "$dir/.runtime_api_port" ] && return 0
    sleep 1
  done
  echo "FATAL: $label never wrote .runtime_api_port after ${tries}s." >&2
  echo "       Refusing to guess a port." >&2
  tail -30 "$log" >&2 2>/dev/null
  return 1
}

assert_port_owned_by() {
  local port="$1" pid="$2" label="$3" holders
  holders="$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null | tr '\n' ' ')"
  case " $holders " in
    *" $pid "*) return 0 ;;
  esac
  echo "FATAL: port $port is held by pid(s) [${holders:-none}], not $label (pid $pid)." >&2
  echo "       Refusing to drive a pond-server this script did not start." >&2
  return 1
}

wait_for_health() {
  local port="$1" label="$2" tries="${3:-180}"
  for _ in $(seq 1 "$tries"); do
    curl -sf -o /dev/null "http://127.0.0.1:$port/api/v1/health" && return 0
    sleep 1
  done
  echo "FATAL: $label never became healthy on port $port after ${tries}s." >&2
  return 1
}

# ── Build ────────────────────────────────────────────────────────────────────
#
# RUSTFLAGS="" matches ci.yml: .cargo/config.toml sets -C target-cpu=native for
# on-device performance, and a native-CPU artifact SIGILLs on another machine in
# a way that reads exactly like a miscompile.
export RUSTFLAGS=""
export SQLX_OFFLINE=true
# No picture-support fetch into the scratch pond: it would cost ~1 GB per run
# and die with the probe, leaving an .incomplete behind.
export POND_DISABLE_MODEL_PROVISIONING=1

BIN="$REPO_ROOT/target/release/pond-server"
if [ "$DO_BUILD" -eq 1 ]; then
  say "Building (release -- a debug binary measures the wrong thing)"
  # Debug builds report inference numbers that are wrong by an order of
  # magnitude and have sent people chasing phantom regressions on the Jetson.
  cargo build --release -p pond-server || exit 1
fi
[ -x "$BIN" ] || { echo "FATAL: no binary at $BIN (drop --no-build?)" >&2; exit 1; }

# ── Start ────────────────────────────────────────────────────────────────────
say "Starting a scratch pond with real weights"
mkdir -p "$DATA_DIR/models/gguf"

# Resolve the model to a real file. TWO identities matter here and conflating
# them breaks the run in two different ways.
#
#   ENTRY -- the name in the models directory, which is what the registry
#            resolves. A pond's `chat_model` is the CANONICAL STEM
#            (`gemma-4-E2B-it`) while the file carries its quant tag
#            (`gemma-4-E2B-it-Q4_K_M.gguf`); `resolve_gguf_filename` bridges
#            those on the Rust side, and this harness has to do the same or it
#            reports FATAL against a model that is sitting right there. It did,
#            on the Orin, immediately after being taught to default to the
#            pond's own chat_model.
#
#   SRC   -- where the bytes actually live. The real registry leaves symlinks
#            into `hf_cache`, so this can be a blob whose basename is a SHA.
#            Linking under that name would give the scratch pond a model no
#            registry can resolve, so the link is always named for the ENTRY.
#
# The glob is written on the FULL PATH, not on a bare filename joined to the
# directory afterwards. A pattern only expands against the current directory,
# and this script `cd`s to the repo root at startup -- so `"$MODEL"-*.gguf`
# matched nothing there, stayed literal, and every candidate was tested as a
# path with an asterisk in it. The failure looked exactly like a missing model
# while the model was in the list the error message printed.
ENTRY=""
for path in "$REAL_MODELS/gguf/$MODEL.gguf" "$REAL_MODELS/gguf/$MODEL"-*.gguf; do
  if [ -e "$path" ]; then ENTRY="$(basename "$path")"; break; fi
done
if [ -z "$ENTRY" ]; then
  echo "FATAL: could not resolve $MODEL to a file under $REAL_MODELS/gguf" >&2
  echo "       (tried '$MODEL.gguf' and '$MODEL-<quant>.gguf'; present:)" >&2
  ls -1 "$REAL_MODELS/gguf" 2>/dev/null | sed 's/^/         /' >&2
  exit 1
fi
SRC="$REAL_MODELS/gguf/$ENTRY"
SRC="$(readlink -f "$SRC" 2>/dev/null || python3 -c "import os,sys;print(os.path.realpath(sys.argv[1]))" "$SRC")"
if [ ! -f "$SRC" ]; then
  echo "FATAL: $ENTRY points at $SRC, which is not a file." >&2
  echo "       A dangling symlink in the models directory -- the blob it names" >&2
  echo "       was pruned from hf_cache." >&2
  exit 1
fi
if ! ln "$SRC" "$DATA_DIR/models/gguf/$ENTRY" 2>/dev/null; then
  # Different filesystem: copy rather than symlink. Slower, still isolated.
  echo "  (hard link failed -- copying $ENTRY; scratch is on another filesystem)"
  cp "$SRC" "$DATA_DIR/models/gguf/$ENTRY" || exit 1
fi
echo "model:     $MODEL   (chosen by: ${MODEL_SOURCE:-unknown})"
echo "weights:   hard-linked from $SRC"
echo "data dir:  $DATA_DIR"

# giap::trace at info carries the per-turn metrics; the adapter at debug carries
# the KV prefill plan and the provider payload size. Both are needed and neither
# is on by default -- goose's own logging is carved down to ERROR.
# NOT --port 0, and not 4000. `bind_with_fallback` returns the port it was
# ASKED for rather than the one the socket got, so `--port 0` publishes a
# literal "0" and every probe then talks to nothing. And 4000..4009 is where a
# real pond lives -- the sibling script once drove one and overwrote four rows.
START_PORT="${PAI_BENCH_PORT_START:-4970}"

POND_DATA_DIR="$DATA_DIR" POND_DEV_ALLOW_LOOPBACK=1 \
RUST_LOG="warn,giap::trace=info,pond_server=info,pond_adapters_goose=debug,goose_local_inference=debug,llama_cpp_2=info" \
  "$BIN" serve --port "$START_PORT" > "$DATA_DIR/server.out" 2>&1 < /dev/zero &
SERVER_PID=$!

wait_for_port_file "$DATA_DIR" "bench server" "$DATA_DIR/server.out" || exit 1
PORT_RESOLVED="$(tr -d ' \n' < "$DATA_DIR/.runtime_api_port")"
wait_for_health "$PORT_RESOLVED" "bench server" || { tail -40 "$DATA_DIR/server.out" >&2; exit 1; }
assert_port_owned_by "$PORT_RESOLVED" "$SERVER_PID" "bench server" || exit 1
echo "port:      $PORT_RESOLVED (published by the server, not assumed)"

# ── Arm the startup-wired capabilities, then restart ─────────────────────────
#
# Two capabilities are decided ONCE at boot and cannot be armed by a running
# process, which cost this script two false SKIPs before anyone noticed:
#
#   giap-orchestrator  `register_giap_extensions` reads `ext_orchestrator_enabled`
#                      at agent-build time. It ships OFF, so the extension is
#                      never registered and `delegate` does not exist in the
#                      model's tool set. A runtime PUT changes nothing.
#   weather            `get_weather` gates on `state.weather_provider`, which
#                      main.rs wires at startup from settings, and answers
#                      `{"enabled":false}` otherwise -- indistinguishable from
#                      the feature being off.
#
# So: boot once to create the database, write the settings, and boot again. The
# model loads lazily on the first turn, so the second boot costs seconds rather
# than a reload. Anything that CAN be set at runtime is left to the probes.
say "Arming the startup-wired capabilities (orchestrator, weather) and restarting"
curl -sf -X PUT "http://127.0.0.1:$PORT_RESOLVED/api/v1/settings" \
  -H 'Content-Type: application/json' \
  -d '{"ext_orchestrator_enabled":true,"weather_enabled":true,
       "weather_latitude":-0.0917,"weather_longitude":34.7680,
       "weather_location_name":"Kisumu"}' -o /dev/null \
  || echo "  (settings PUT failed -- PAI-6 and PAI-2 will report SKIP and say why)"

kill -9 "$SERVER_PID" 2>/dev/null
wait "$SERVER_PID" 2>/dev/null
rm -f "$DATA_DIR/.runtime_api_port"

POND_DATA_DIR="$DATA_DIR" POND_DEV_ALLOW_LOOPBACK=1 \
RUST_LOG="warn,giap::trace=info,pond_server=info,pond_adapters_goose=debug,goose_local_inference=debug,llama_cpp_2=info" \
  "$BIN" serve --port "$START_PORT" >> "$DATA_DIR/server.out" 2>&1 < /dev/zero &
SERVER_PID=$!

wait_for_port_file "$DATA_DIR" "bench server (armed)" "$DATA_DIR/server.out" || exit 1
PORT_RESOLVED="$(tr -d ' \n' < "$DATA_DIR/.runtime_api_port")"
wait_for_health "$PORT_RESOLVED" "bench server (armed)" || { tail -40 "$DATA_DIR/server.out" >&2; exit 1; }
assert_port_owned_by "$PORT_RESOLVED" "$SERVER_PID" "bench server (armed)" || exit 1
if grep -q "giap-orchestrator" "$DATA_DIR/server.out"; then
  echo "armed:     giap-orchestrator registered"
else
  echo "armed:     giap-orchestrator NOT registered -- PAI-6 will say so"
fi

# ── Probe ────────────────────────────────────────────────────────────────────
say "Driving the eight capabilities"
PAI_BENCH_PORT="$PORT_RESOLVED" \
PAI_BENCH_DATA="$DATA_DIR" \
PAI_BENCH_MODEL="$MODEL" \
PAI_BENCH_SLOW="$SLOW" \
PAI_BENCH_ONLY="$ONLY" \
PAI_BENCH_JSON="$JSON_OUT" \
  python3 "$REPO_ROOT/scripts/pai_bench.py"
RC=$?

# ── Dig the log ──────────────────────────────────────────────────────────────
#
# For more than this run's own features. WARN and ERROR lines that were already
# there are still findings, and a benchmark that only reports its own numbers
# will happily do so on a pond that is failing at something else.
say "log dig"
grep -E "WARN|ERROR" "$DATA_DIR/server.out" 2>/dev/null \
  | sed -E 's/.*(WARN|ERROR)/\1/' | sort | uniq -c | sort -rn | head -12
grep -cE "WARN|ERROR" "$DATA_DIR/server.out" 2>/dev/null | xargs -I{} echo "({} total)"

exit $RC
