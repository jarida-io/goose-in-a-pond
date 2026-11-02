#!/usr/bin/env bash
# Builds a pinned upstream llama.cpp beside the serving pond, for the P0 EmbeddingGemma 2 harness.
# Usage: build-llama.sh [workdir] [cpu|cuda]   (default ~/p0-embed cpu)
# With GGML_BACKEND_DL the CUDA backend is a module, so the CPU tools are usable (and
# measurable) before the hour-long single-job CUDA build is even started.
set -euo pipefail

# The first upstream tree with gemma-embedding2 and the no-cache attention padding fix.
SHA=0ec4027a2a0183de87c007069adddc06e665f7de
WORK=${1:-$HOME/p0-embed}
STAGE=${2:-cpu}
SRC=$WORK/llama.cpp
JOBS=${JOBS:-1}
# Below this MemAvailable the build pauses for a while, giving the kernel time to swap
# it out instead of the pond; a timed resume, because a stopped job still holds its RSS.
PAUSE_MB=${PAUSE_MB:-150}
PAUSE_S=${PAUSE_S:-20}

mkdir -p "$WORK"
if [ ! -d "$SRC/.git" ]; then
  git init -q "$SRC"
  git -C "$SRC" remote add origin https://github.com/ggml-org/llama.cpp
fi
if [ "$(git -C "$SRC" rev-parse HEAD 2>/dev/null)" != "$SHA" ]; then
  git -C "$SRC" fetch -q --depth 1 origin "$SHA"
  git -C "$SRC" checkout -q FETCH_HEAD
fi

export PATH=/usr/local/cuda/bin:$PATH
# ggml depends on every enabled backend module, so CUDA stays off until its own stage.
CUDA=OFF
[ "$STAGE" = cuda ] && CUDA=ON
# FA kernels are the costliest CUDA objects and an embedding graph does not need them;
# NO_VMM matches the pond's llama-cpp-2 cuda-no-vmm feature.
cmake -S "$SRC" -B "$SRC/build" \
  -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_SHARED_LIBS=ON -DGGML_BACKEND_DL=ON -DGGML_NATIVE=ON \
  -DGGML_CUDA=$CUDA -DCMAKE_CUDA_ARCHITECTURES=87 -DGGML_CUDA_FA=OFF \
  -DGGML_CUDA_NO_VMM=ON -DGGML_CUDA_NCCL=OFF \
  -DLLAMA_OPENSSL=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_UI=OFF

mem_avail_mb() { awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo; }

if [ "$STAGE" = cuda ]; then
  TARGETS=(ggml-cuda)
else
  TARGETS=(llama mtmd ggml-cpu llama-server llama-embedding llama-quantize)
fi
setsid nice -n 19 ionice -c3 cmake --build "$SRC/build" -j"$JOBS" --target "${TARGETS[@]}" &
BUILD=$!
PGID=$(ps -o pgid= "$BUILD" | tr -d ' ')
low=$(mem_avail_mb)
while kill -0 "$BUILD" 2>/dev/null; do
  m=$(mem_avail_mb)
  [ "$m" -lt "$low" ] && low=$m
  if [ "$m" -lt "$PAUSE_MB" ]; then
    kill -STOP -- "-$PGID" 2>/dev/null || true
    echo "watchdog: paused ${PAUSE_S}s at MemAvailable=${m} MB"
    sleep "$PAUSE_S"
    kill -CONT -- "-$PGID" 2>/dev/null || true
  fi
  sleep 2
done
echo "lowest MemAvailable during the build: ${low} MB"
wait "$BUILD"
echo "BUILD_EXIT=$?"
ls -la "$SRC/build/bin"
