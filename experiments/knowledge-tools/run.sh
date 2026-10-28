#!/usr/bin/env bash
# Run the bench over the Mac's GGUF collection, ONE llama-server at a time (timing stays honest
# and two 4B models do not fight for unified memory).
#
#   ./run.sh                     every model, every experiment
#   ./run.sh e1 gemma-4-E2B-it-qat-UD-Q4_K_XL
#   ONLY=r01,c01 ./run.sh e1     a subset of cases
#   PAYLOADS=payloads/<commit> OUT=results-<commit> ./run.sh e1   replay another capture
#   MMPROJ=<model's mmproj.gguf> ./run.sh e9 <one model>             E9's attached pictures need it
#   LLAMA_SERVER=~/llama.cpp/build/bin/llama-server ./run.sh ...     a build not on PATH
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODELS_DIR="${GIAP_MODELS:-$HOME/Library/Application Support/goose-in-a-pond/models/gguf}"
PORT="${PORT:-8095}"
EXPS="${1:-e1 e3 e2}"
shift || true
MODELS=("$@")
[ ${#MODELS[@]} -gt 0 ] || MODELS=(
  gemma-4-E2B-it-qat-UD-Q4_K_XL
  gemma-4-E4B-it-qat-UD-Q4_K_XL
  Llama-3.2-3B-Instruct-Q4_K_M
  granite-4.1-3b-Q4_K_M
  NVIDIA-Nemotron3-Nano-4B-Q4_K_M
  Nanbeige_Nanbeige4.2-3B-Q4_K_M
)
OUT="${OUT:-$HERE/results}"
mkdir -p "$HERE/logs" "$OUT"
LLAMA_SERVER="${LLAMA_SERVER:-llama-server}"
command -v "$LLAMA_SERVER" >/dev/null || { echo "llama-server not found (brew install llama.cpp)"; exit 1; }

stop() { pkill -f "llama-server.*--port $PORT" 2>/dev/null; sleep 1; }
trap stop EXIT

for m in "${MODELS[@]}"; do
  gguf="$MODELS_DIR/$m.gguf"
  [ -e "$gguf" ] || { echo "skip $m: not at $gguf"; continue; }
  stop
  "$LLAMA_SERVER" -m "$gguf" ${MMPROJ:+--mmproj "$MMPROJ"} --port "$PORT" -c "${CTX:-16384}" -ngl 99 --jinja \
    --reasoning-budget "$( [ "${THINK:-0}" = 1 ] && echo -1 || echo 0 )" \
    > "$HERE/logs/$m.log" 2>&1 &
  for _ in $(seq 1 120); do curl -sf -m 2 "http://127.0.0.1:$PORT/health" >/dev/null && break; sleep 1; done
  curl -sf -m 2 "http://127.0.0.1:$PORT/health" >/dev/null || { echo "$m did not start; logs/$m.log"; continue; }
  for e in $EXPS; do
    echo "=== $m $e"
    python3 -I "$HERE/bench.py" --exp "$e" --model "$m" --url "http://127.0.0.1:$PORT" --out "$OUT" \
      ${ONLY:+--only "$ONLY"} > "$HERE/logs/$m-$e$(basename "$OUT" | sed 's/^results//').txt" 2>&1
    tail -1 "$HERE/logs/$m-$e$(basename "$OUT" | sed 's/^results//').txt"
  done
done
