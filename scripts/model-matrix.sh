#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# model-matrix.sh — drive SEVERAL models through the same turns and report what
# each one actually did.
#
#   scripts/model-matrix.sh                        every GGUF that can chat
#   scripts/model-matrix.sh --models a,b,c         just these
#   scripts/model-matrix.sh --no-build             use the existing binary
#   scripts/model-matrix.sh --json OUT.json        machine-readable results
#   scripts/model-matrix.sh --thinking auto|on|off which thinking_mode to set
#   scripts/model-matrix.sh --tools all|relevant   which tool_selection_mode
#   scripts/model-matrix.sh --bin PATH             drive a specific pond-server
#   scripts/model-matrix.sh --keep                 keep each scratch pond for inspection
#
# --bin is what makes a before/after honest. Keep a copy of the OLD binary and
# point this at it, rather than rebuilding between the two runs: a rebuild in
# between means the two halves were measured minutes apart on a machine whose
# thermal and cache state moved, and it makes it impossible to re-run the
# "before" once the source has changed.
#
# WHY THIS EXISTS, SEPARATELY FROM pai-bench.sh
#
# `pai-bench.sh` answers "do the eight capabilities work on this hardware", for
# ONE model. This answers "does the pond work for a model it was not built
# around", across many — which is a different question, and the one that
# catches a layer still deciding from a filename.
#
# The columns that matter are not the speed ones:
#
#   reengage   attempts BEYOND the first. A model that reasons but was given no
#              <thinking> section ends its turn silent, gets steered back with
#              EMPTY_TURN_STEER, and runs the WHOLE turn again — prefill and
#              tool schemas included. It is the hidden cost of a capability
#              mismatch and it reads as "the pond is slow", never as a bug.
#   tool       whether the tool-using turn actually called a tool. A model that
#              answers a weather question from imagination looks identical to
#              one that answered it correctly, unless you check this.
#
# Every model gets its own scratch POND_DATA_DIR with ONE GGUF hard-linked in.
# Never symlinked: a symlinked models/ IS the real directory, and the startup
# hf_cache migration MOVES real files out of it. That cost 3.1 GB once.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DO_BUILD=1
MODELS=""
JSON_OUT=""
THINKING="auto"
TOOLS="all"
BIN_OVERRIDE=""
KEEP=0
PORT="${PORT:-4988}"

while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) DO_BUILD=0 ;;
    --models)   MODELS="${2:-}"; shift ;;
    --json)     JSON_OUT="${2:-}"; shift ;;
    --thinking) THINKING="${2:-}"; shift ;;
    --tools)    TOOLS="${2:-}"; shift ;;
    --bin)      BIN_OVERRIDE="${2:-}"; shift ;;
    --keep)     KEEP=1 ;;
    --port)     PORT="${2:-}"; shift ;;
    -h|--help)  sed -n '2,34p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
  shift
done

case "$(uname -s)" in
  Darwin) REAL_MODELS="$HOME/Library/Application Support/goose-in-a-pond/models" ;;
  *)      REAL_MODELS="$HOME/.local/share/goose-in-a-pond/models" ;;
esac

if [ ! -d "$REAL_MODELS/gguf" ]; then
  echo "FATAL: no model directory at $REAL_MODELS/gguf" >&2; exit 1
fi

# Models that cannot serve a chat turn regardless of any fix here: the MTP
# speculative-decode drafts carry no chat template at all, and the 270m is a
# tool-calling experiment, not an assistant. Excluding them keeps the table
# about model agnosticity rather than about known non-models.
SKIP_RE='assistant|old_functiongemma'

if [ -z "$MODELS" ]; then
  MODELS="$(ls -1 "$REAL_MODELS/gguf"/*.gguf 2>/dev/null \
    | xargs -n1 basename | sed 's/\.gguf$//' \
    | grep -Ev "$SKIP_RE" | paste -sd, -)"
fi

if [ -n "$BIN_OVERRIDE" ]; then DO_BUILD=0; fi
if [ "$DO_BUILD" = "1" ]; then
  echo "building pond-server ..."
  SQLX_OFFLINE=true cargo build -p pond-server 2>&1 | tail -3
fi
BIN="${BIN_OVERRIDE:-$REPO_ROOT/target/debug/pond-server}"
[ -x "$BIN" ] || { echo "FATAL: no binary at $BIN" >&2; exit 1; }

# An ONNX Runtime already present on this machine, so each model does not
# re-download one into its own throwaway data dir.
# HIGHEST version, not the first the glob happens to name.
#
# A pond's lib/ can hold several. Taking the first match picked
# libonnxruntime.1.22.0.dylib over 1.24.2 — lexicographic order — and the
# EMBEDDER then failed to initialise with "ONNX Runtime may be
# version-incompatible (need ORT 1.24.2)". Nothing about chat broke, so the
# runs looked fine; what broke was `tool_selection_mode = "relevant"`, which
# needs embeddings to score groups and correctly widens to every tool when it
# cannot get them. Three measurements of "relevant" were really measurements of
# "all", and the log said `reason="no_embedder"` the whole time.
if [ -z "${ORT_DYLIB_PATH:-}" ]; then
  for cand in $(ls -1 \
        "$HOME/Library/Application Support/goose-in-a-pond/lib/"libonnxruntime.*.dylib \
        "$HOME/.local/share/goose-in-a-pond/lib/"libonnxruntime.*.so \
        2>/dev/null | sort -V -r) \
      /opt/homebrew/lib/libonnxruntime.dylib \
      /usr/local/lib/libonnxruntime.dylib \
      /usr/lib/libonnxruntime.so; do
    if [ -e "$cand" ]; then ORT_DYLIB_PATH="$cand"; export ORT_DYLIB_PATH; break; fi
  done
fi
[ -n "${ORT_DYLIB_PATH:-}" ] && echo "onnxruntime: $ORT_DYLIB_PATH (reused)"

RESULTS_DIR="$(mktemp -d)"
echo "results: $RESULTS_DIR"

# The three turns, chosen so each isolates one thing:
#   1. plain     — does it answer at all, and what does a COLD turn cost
#   2. tool      — does it call a tool, or invent the answer
#   3. followup  — does the KV prefix survive into turn 2 (reuse TTFT)
PROMPT_1='Say hello in one short sentence.'
PROMPT_2='What is the weather right now? Use your tools.'
PROMPT_3='Thanks. Now say goodbye in one short sentence.'

run_model() {
  local model="$1"
  local data_dir; data_dir="$(mktemp -d)"
  local log="$RESULTS_DIR/$model.log"
  local out="$RESULTS_DIR/$model.json"

  mkdir -p "$data_dir/models/gguf" "$data_dir/bin"

  # Pre-seed espeak-ng-data. A fresh data dir otherwise downloads ~18 MB of it
  # during startup, per model, and the health check times out waiting. Safe to
  # SYMLINK unlike the models directory: the hf_cache migration walks only
  # `models/gguf` and MOVES what it finds, which is why a symlinked models/ once
  # ate a real pond's weights. Nothing rewrites `bin/`.
  for esp in /opt/homebrew/share/espeak-ng-data /usr/share/espeak-ng-data \
             "$HOME/Library/Application Support/goose-in-a-pond/bin/espeak-ng-data" \
             "$HOME/.local/share/goose-in-a-pond/bin/espeak-ng-data"; do
    if [ -d "$esp" ]; then ln -s "$esp" "$data_dir/bin/espeak-ng-data" 2>/dev/null; break; fi
  done

  local entry=""
  for path in "$REAL_MODELS/gguf/$model.gguf" "$REAL_MODELS/gguf/$model"-*.gguf; do
    [ -e "$path" ] && { entry="$(basename "$path")"; break; }
  done
  if [ -z "$entry" ]; then
    echo "  SKIP $model — no file"; rm -rf "$data_dir"; return
  fi

  local src="$REAL_MODELS/gguf/$entry"
  src="$(python3 -c 'import os,sys;print(os.path.realpath(sys.argv[1]))' "$src")"
  [ -f "$src" ] || { echo "  SKIP $model — dangling symlink"; rm -rf "$data_dir"; return; }
  ln "$src" "$data_dir/models/gguf/$entry" 2>/dev/null \
    || cp "$src" "$data_dir/models/gguf/$entry" \
    || { echo "  SKIP $model — could not stage"; rm -rf "$data_dir"; return; }

  # Optional pre-seeding of Kokoro and any MTP drafter, for the reason
  # espeak-ng-data and ORT_DYLIB_PATH are pre-seeded above: a scratch pond
  # otherwise fetches ~155 MB of Kokoro and a 57 MB drafter per model per run,
  # and on a slow link that exhausts the 360 s health window before the model is
  # ever loaded. OFF by default until it is understood -- staging Kokoro made
  # startup go silent after the ONNX download and never become healthy, for both
  # models, where the same run without it worked. Set MATRIX_PRESEED=1 to try it.
  #
  # Hard links where the filesystem allows, and realpath FIRST: entries under
  # models/gguf are often symlinks into hf_cache, and the startup migration
  # MOVES what it finds in models/gguf -- the same trap the staging above avoids.
  if [ "${MATRIX_PRESEED:-0}" = "1" ]; then
    if [ -d "$REAL_MODELS/kokoro" ]; then
      cp -al "$REAL_MODELS/kokoro" "$data_dir/models/" 2>/dev/null \
        || cp -R "$REAL_MODELS/kokoro" "$data_dir/models/" 2>/dev/null || true
    fi
    for drafter in "$REAL_MODELS/gguf"/mtp-*.gguf; do
      [ -e "$drafter" ] || continue
      local dreal; dreal="$(python3 -c 'import os,sys;print(os.path.realpath(sys.argv[1]))' "$drafter")"
      [ -f "$dreal" ] || continue
      ln "$dreal" "$data_dir/models/gguf/$(basename "$drafter")" 2>/dev/null \
        || cp "$dreal" "$data_dir/models/gguf/$(basename "$drafter")" 2>/dev/null || true
    done
  fi

  # Refuse to start on an occupied port. Paired with the ownership check below:
  # this catches the leak before it can be measured, that one catches ours dying.
  if curl -sf -o /dev/null --max-time 2 "http://127.0.0.1:$PORT/api/v1/health"; then
    echo "  FAIL $model — port $PORT is already serving; a leaked pond-server would be measured instead"
    rm -rf "$data_dir"; return
  fi

  # ORT_DYLIB_PATH is exported at the top when a runtime was found on this
  # machine, and inherited from here. Without it every model re-downloads ~30 MB
  # into its own scratch dir, because the dir is wiped between models -- minutes
  # per model of the harness measuring the network rather than the model.
  # Deliberately NOT an inline `VAR=x cmd` prefix: the macOS path contains a
  # space ("Application Support") and `${VAR:+VAR="$VAR"}` does not survive word
  # splitting, which turned the assignment into a command and failed every model
  # instantly with "No such file or directory".
  POND_DATA_DIR="$data_dir" POND_DEV_ALLOW_LOOPBACK=1 \
    RUST_LOG="${MATRIX_RUST_LOG:-warn,giap::trace=info,pond_adapters_goose=debug,pond_adapters_local_inference=debug,goose_local_inference=debug}" \
    "$BIN" serve --port "$PORT" > "$log" 2>&1 &
  local pid=$!

  # The health check below trusts whatever answers on $PORT. A pond-server
  # leaked by an earlier run answers it, reports itself onboarded, accepts the
  # activate -- and then fails every turn with "Model not downloaded", because
  # its own scratch data dir was removed when that run ended. Two full matrix
  # runs were recorded as data before that was spotted. So: our process, or
  # nothing.
  local ready=0
  for _ in $(seq 1 180); do
    if curl -sf -o /dev/null "http://127.0.0.1:$PORT/api/v1/health"; then
      if kill -0 "$pid" 2>/dev/null; then ready=1; break; fi
      echo "  FAIL $model — something else is serving port $PORT; our server is gone"
      rm -rf "$data_dir"; return
    fi
    kill -0 "$pid" 2>/dev/null || break
    sleep 2
  done
  if [ "$ready" != "1" ]; then
    echo "  FAIL $model — server never became healthy (see $log)"
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; rm -rf "$data_dir"; return
  fi

  # Settings BEFORE onboarding: `POST /onboard/complete` refuses while any
  # required field is unset, and `chat_model` is one of them.
  curl -s -X PUT "http://127.0.0.1:$PORT/api/v1/settings" \
    -H 'Content-Type: application/json' \
    -d "{\"chat_provider\":\"local\",\"chat_model\":\"$model\",\"thinking_mode\":\"$THINKING\",\"tool_selection_mode\":\"$TOOLS\",\"show_turn_stats\":true}" \
    > /dev/null

  # Finish onboarding through the product's own public endpoint rather than by
  # writing `onboarding_state` directly. The direct INSERT this replaces could
  # never succeed: `id` is `INTEGER PRIMARY KEY CHECK (id = 1)` and the server
  # seeds row 1 during startup, so an INSERT run after the health check always
  # hit the constraint -- and `2>/dev/null` made the failure look like success.
  # Every turn then returned `onboarding_required` and the parser, which counts
  # only `data:` lines, recorded nulls. A whole matrix of empty rows.
  curl -s -X POST "http://127.0.0.1:$PORT/api/v1/onboard/complete" \
    -H 'Content-Type: application/json' -d '{}' > /dev/null

  # Assert it took. A model that cannot be driven must not be reported as a
  # model that said nothing.
  if ! curl -s "http://127.0.0.1:$PORT/api/v1/onboard/status" | grep -q '"onboarded":true'; then
    echo "  FAIL $model — onboarding did not complete; turns would all 403 (see $log)"
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; rm -rf "$data_dir"; return
  fi

  # Register the staged GGUF, then assign it the chat role. The file being on
  # disk is not enough: the engine resolves a model through its CATALOGUE row,
  # and a scratch pond starts with an empty `models` table holding only the
  # seeded ASR and TTS entries. A model the catalogue does not already know --
  # which is the entire point of this script -- otherwise answers every turn
  # with "Model not downloaded: <name>" about a file that is right there, and
  # the parser records that sentence as the reply. Measured: 203 reply chars
  # per turn, no stats, and a `done` line.
  #
  # `activate` and not `scan` alone: `ModelRouter` is built at startup, and
  # activate is the call that rebuilds it for LLM roles.
  curl -s -X POST "http://127.0.0.1:$PORT/api/v1/models/scan" > /dev/null
  curl -s -X POST "http://127.0.0.1:$PORT/api/v1/models/gguf/$model/activate" \
    -H 'Content-Type: application/json' -d '{"role":"chat"}' > /dev/null

  if ! curl -s "http://127.0.0.1:$PORT/api/v1/models/active-roles" | grep -q "gguf/$model"; then
    echo "  FAIL $model — not assigned to the chat role after scan+activate (see $log)"
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; rm -rf "$data_dir"; return
  fi

  local sid="matrix-$$"
  local i=0
  echo "[" > "$out"
  for prompt in "$PROMPT_1" "$PROMPT_2" "$PROMPT_3"; do
    i=$((i+1))
    local raw="$RESULTS_DIR/$model.turn$i.sse"
    curl -s -N -X POST "http://127.0.0.1:$PORT/api/v1/chat/stream" \
      -H 'Content-Type: application/json' \
      -d "$(python3 -c 'import json,sys;print(json.dumps({"message":sys.argv[1],"session_id":sys.argv[2]}))' "$prompt" "$sid")" \
      --max-time 420 > "$raw" 2>&1
    [ "$i" = "1" ] || echo "," >> "$out"
    python3 - "$raw" "$i" >> "$out" <<'PY'
import json, sys
raw, turn = sys.argv[1], int(sys.argv[2])
stats, text, tools = None, [], []
for line in open(raw, errors="replace"):
    line = line.strip()
    if not line.startswith("data:"):
        continue
    try:
        ev = json.loads(line[5:].strip())
    except Exception:
        continue
    t = ev.get("type")
    if t == "turn_stats":
        stats = ev
    elif t == "text":
        text.append(ev.get("content") or "")
    elif t == "tool_call":
        tools.append(ev.get("tool") or "?")
s = stats or {}
print(json.dumps({
    "turn": turn,
    "ttft_ms": s.get("ttft_ms"),
    "prefill_ms": s.get("prefill_ms"),
    "decode_ms": s.get("decode_ms"),
    "model_load_ms": s.get("model_load_ms"),
    "prompt_tokens": s.get("prompt_tokens"),
    "completion_tokens": s.get("completion_tokens"),
    "reasoning_tokens": s.get("reasoning_tokens"),
    "reengagements": s.get("reengagements"),
    "tools_called": tools,
    "reply_chars": len("".join(text)),
}))
PY
  done
  echo "]" >> "$out"

  # Stop the server and do not hang if it declines to go. A bare
  # `kill; wait` hung a completed run indefinitely: the child holds the model
  # and an audio device, and a TERM it does not act on leaves `wait` blocking
  # forever with every result already on disk.
  kill "$pid" 2>/dev/null
  for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
  kill -9 "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  if [ "$KEEP" = "1" ]; then
    # goose keeps its own store under GOOSE_PATH_ROOT, which GIAP points inside
    # the data dir — so the assembled prompt each turn actually received only
    # survives if the scratch pond does.
    echo "  kept: $data_dir"
  else
    rm -rf "$data_dir"
  fi
  echo "  done $model"
}

echo "models: $MODELS"
echo "thinking_mode: $THINKING"
echo "tool_selection_mode: $TOOLS"
echo "binary: $BIN"
echo
IFS=',' read -ra LIST <<< "$MODELS"
for m in "${LIST[@]}"; do
  [ -n "$m" ] || continue
  echo "── $m"
  run_model "$m"
done

echo
python3 - "$RESULTS_DIR" "${JSON_OUT:-}" <<'PY'
import json, os, sys, glob
d = sys.argv[1]
out = sys.argv[2] if len(sys.argv) > 2 and sys.argv[2] else None
rows, allres = [], {}
for f in sorted(glob.glob(os.path.join(d, "*.json"))):
    model = os.path.basename(f)[:-5]
    try:
        turns = json.load(open(f))
    except Exception:
        rows.append((model, "PARSE-FAIL", "", "", "", "", "", ""))
        continue
    allres[model] = turns
    t1 = turns[0] if turns else {}
    t2 = turns[1] if len(turns) > 1 else {}
    t3 = turns[2] if len(turns) > 2 else {}
    reeng = sum((t.get("reengagements") or 0) for t in turns)
    tools = ",".join(x for t in turns for x in (t.get("tools_called") or []))
    def ms(v): return f"{v/1000:.2f}s" if isinstance(v, (int, float)) else "—"
    rows.append((
        model,
        ms(t1.get("ttft_ms")),
        ms(t3.get("ttft_ms")),
        str(t1.get("prompt_tokens") or "—"),
        ms(t1.get("prefill_ms")),
        str(reeng),
        (tools or "NONE"),
        str(sum((t.get("reply_chars") or 0) for t in turns)),
    ))
hdr = ("model", "ttft cold", "ttft reuse", "prompt tok", "prefill", "reengage", "tools called", "reply chars")
w = [max(len(str(r[i])) for r in ([hdr] + rows)) for i in range(len(hdr))]
def line(r): return "  ".join(str(r[i]).ljust(w[i]) for i in range(len(hdr)))
print(line(hdr)); print("  ".join("-" * x for x in w))
for r in rows: print(line(r))
if out:
    json.dump(allres, open(out, "w"), indent=2)
    print(f"\nwrote {out}")
PY
