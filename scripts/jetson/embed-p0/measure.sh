#!/usr/bin/env bash
# Runs the P0 measurements beside the serving pond, each step behind preflight.sh.
# Usage: measure.sh [workdir] STEP...   STEP: recall | cpu | gpu | encoders | images | litert
set -uo pipefail
WORK=${1:-$HOME/p0-embed}
shift || true
H=$WORK/harness
B=$WORK/llama.cpp/build/bin
VPY="$WORK/venv/bin/python -I"
WATCH="/usr/bin/python3 $H/memwatch.py"
Q8=$WORK/gguf/eg2-text-q8_0.gguf
MINILM=$(ls -d "${POND_DATA_DIR:-$HOME/.local/share/goose-in-a-pond}"/models/embedding/models--Qdrant--all-MiniLM-L6-v2-onnx/snapshots/*/ | head -1)
SETS="reach pctx sw"
cd "$WORK" && mkdir -p out runs

gate() { bash "$H/preflight.sh" "$1" || { echo "skipped: $2"; return 1; }; }

step_recall() {
  for s in $SETS; do for k in docs queries; do
    $VPY "$H/minilm_embed.py" "$MINILM" embed "data/$s.$k.minilm.txt" "out/$s.$k.minilm.f32"
    gate 350 "eg2 $s $k" && $B/eg2bench "$Q8" --dev cpu --ctx 1024 --seq 32 --quiet embed \
      "data/$s.$k.eg2.txt" "out/$s.$k.eg2-q8_0-cpu.f32" 2>/dev/null
  done; done
  for s in $SETS; do
    $VPY "$H/recall.py" data "$s" 768 "eg2-q8-cpu=out/$s.queries.eg2-q8_0-cpu.f32,out/$s.docs.eg2-q8_0-cpu.f32,512,256,128" \
      "minilm@384=out/$s.queries.minilm.f32,out/$s.docs.minilm.f32"
  done
  $VPY "$H/pooled_recall.py" data out minilm 384
  $VPY "$H/pooled_recall.py" data out eg2-q8_0-cpu 768 256
}

step_cpu() {
  gate 400 "cpu latency" && $WATCH runs/cpu-q8-q256-repack.json -- $B/eg2bench "$Q8" --dev cpu --threads 4 \
    --ctx 256 --seq 4 --hold 4 --pageout latency data/all.queries.eg2.txt 200
  gate 400 "cpu latency, no repack" && $WATCH runs/cpu-q8-q256-norepack.json -- $B/eg2bench "$Q8" --dev cpu \
    --threads 4 --ctx 256 --seq 4 --repack 0 --hold 4 --pageout latency data/all.queries.eg2.txt 200
  gate 450 "cpu throughput" && $WATCH runs/cpu-q8-c1024-throughput.json -- $B/eg2bench "$Q8" --dev cpu \
    --threads 4 --ctx 1024 --seq 64 --hold 3 throughput data/all.docs.eg2.txt 1,2,4,8,16,24,32 5
  $VPY "$H/minilm_embed.py" "$MINILM" latency data/all.queries.minilm.txt 200
  gate 200 "minilm memory" && $WATCH runs/minilm-cpu-mem.json -- $VPY "$H/minilm_embed.py" "$MINILM" mem \
    data/all.queries.minilm.txt
}

step_gpu() {
  gate 400 "gpu latency" && $WATCH runs/gpu-q8-q256.json -- $B/eg2bench "$Q8" --dev gpu --ctx 256 --seq 4 \
    --hold 4 --pageout latency data/all.queries.eg2.txt 200
  gate 400 "gpu latency, mapped token table" && $WATCH runs/gpu-q8-q256-nohost-mmap.json -- $B/eg2bench "$Q8" \
    --dev gpu --ctx 256 --seq 4 --no-host --hold 4 --pageout latency data/all.queries.eg2.txt 200
  gate 450 "gpu throughput" && $WATCH runs/gpu-q8-c1024-throughput.json -- $B/eg2bench "$Q8" --dev gpu \
    --ctx 1024 --seq 64 --no-host --hold 3 throughput data/all.docs.eg2.txt 1,2,4,8,16,24,32,48,64 5
  for s in $SETS; do for k in docs queries; do
    gate 400 "gpu embed" && $B/eg2bench "$Q8" --dev gpu --ctx 1024 --seq 32 --no-host --quiet embed \
      "data/$s.$k.eg2.txt" "out/$s.$k.eg2-q8_0-gpu.f32" 2>/dev/null
  done; done
}

step_encoders() {
  local im
  im=$(ls images/img*.jpg | tr '\n' ' ')
  gate 400 "vision encoder" && $WATCH runs/mtmd-cpu-vision-q8-140.json -- $B/mtmdbench "$Q8" \
    gguf/mmproj-eg2-q8_0-vision.gguf --threads 4 --max-tokens 140 --hold 4 $im
  gate 450 "audio encoder" && $WATCH runs/mtmd-cpu-audio-q8.json -- $B/mtmdbench "$Q8" \
    gguf/mmproj-eg2-q8_0-audio.gguf --threads 4 --hold 4 audio/jfk.wav audio/jfk.wav audio/jfk.wav
}

step_images() {
  gate 350 "image queries" && $WATCH runs/srv-cpu-text.json -- $VPY "$H/server_bench.py" $B/llama-server 8391 \
    --hold 4 texts data/img.queries.eg2.txt out/img.queries.eg2-srv-cpu.f32 -- -m "$Q8" --embeddings \
    -c 1024 -b 1024 -ub 1024 -np 1 -t 4 --no-warmup
  gate 560 "images" && $WATCH runs/srv-cpu-text-vision140.json -- $VPY "$H/server_bench.py" $B/llama-server 8391 \
    --hold 3 images images "$(cat data/img.ids)" out/img.docs.eg2-srv-cpu-v140.f32 -- -m "$Q8" \
    --mmproj gguf/mmproj-eg2-q8_0-vision.gguf --embeddings -c 512 -b 512 -ub 512 -np 1 -t 4 --no-repack \
    -fa on --image-max-tokens 140 --no-warmup
  $VPY "$H/recall.py" data img 768 "eg2-srv-cpu-v140=out/img.queries.eg2-srv-cpu.f32,out/img.docs.eg2-srv-cpu-v140.f32,256"
}

step_litert() {
  local m=$WORK/litert/embeddinggemma-2-text-270m.litertlm
  gate 450 "litert cpu" && $WATCH runs/litert270-cpu-latency.json -- $VPY "$H/litert_bench.py" "$m" \
    --backend cpu --threads 4 --hold 4 latency data/all.queries.eg2.txt 100
  gate 500 "litert gpu" && $WATCH runs/litert270-gpu-latency.json -- $VPY "$H/litert_bench.py" "$m" \
    --backend gpu --hold 4 latency data/all.queries.eg2.txt 100
  for s in $SETS; do for k in docs queries; do
    gate 450 "litert embed" && $VPY "$H/litert_bench.py" "$m" --backend cpu --threads 4 embed \
      "data/$s.$k.eg2.txt" "out/$s.$k.eg2-litert270-cpu.f32"
  done; done
  $VPY "$H/pooled_recall.py" data out eg2-litert270-cpu 768 256
}

for step in "$@"; do "step_$step"; done
