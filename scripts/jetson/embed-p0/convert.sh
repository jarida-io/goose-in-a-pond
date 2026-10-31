#!/usr/bin/env bash
# Converts Google's google/embeddinggemma-2 safetensors to GGUF with the pinned llama.cpp converter.
# Usage: convert.sh [workdir]   (expects workdir/hf/embeddinggemma-2, workdir/venv, workdir/llama.cpp)
set -euo pipefail
WORK=${1:-$HOME/p0-embed}
SRC=$WORK/hf/embeddinggemma-2
OUT=$WORK/gguf
mkdir -p "$OUT"
cd "$WORK/llama.cpp"

run() {
  # A conversion briefly holds a whole f32 copy of the 268 MB token table; pause it, not the pond.
  nice -n 19 "$WORK/venv/bin/python" convert_hf_to_gguf.py "$SRC" "$@" &
  local pid=$!
  while kill -0 "$pid" 2>/dev/null; do
    if [ "$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)" -lt 120 ]; then
      kill -STOP "$pid"; sleep 15; kill -CONT "$pid"
    fi
    sleep 1
  done
  wait "$pid"
}

[ -f "$OUT/eg2-text-bf16.gguf" ]  || run --outtype bf16 --outfile "$OUT/eg2-text-bf16.gguf"
[ -f "$OUT/eg2-text-q8_0.gguf" ]  || run --outtype q8_0 --outfile "$OUT/eg2-text-q8_0.gguf"
[ -f "$OUT/mmproj-eg2-bf16.gguf" ] || run --mmproj --outtype bf16 --outfile "$OUT/mmproj-eg2-bf16.gguf"
[ -f "$OUT/mmproj-eg2-q8_0.gguf" ] || run --mmproj --outtype q8_0 --outfile "$OUT/mmproj-eg2-q8_0.gguf"
ls -la "$OUT"
