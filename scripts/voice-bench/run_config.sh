#!/usr/bin/env bash
# run_config.sh NAME MODEL PASS MIC [ear_bench flags...]: one config over a pass's recordings, then
# score.py into out/NAME/score.json. PROGS picks the recordings; VARIANTS the learned wake variants.
# @PROG@ in a flag becomes each recording's program name (--with rec/A-@PROG@-usb.wav).
set -u
B=$(cd "$(dirname "$0")" && pwd)
BENCH=${BENCH:-$B}
BIN=${EAR_BENCH:-target/release/examples/ear_bench}
SILERO=${SILERO:?the Silero ONNX the pond uses}
name=$1 model=$2 pass=$3 mic=$4
shift 4
variants=()
for v in ${VARIANTS:-}; do variants+=(--variant "${v//_/ }"); done
mkdir -p "$BENCH/out/$name"
for prog in ${PROGS:-quiet babble10 babble3 noise silence}; do
  rec=$BENCH/rec/$pass-$prog-$mic.wav
  [ -f "$rec" ] || continue
  manifest=()
  case $prog in quiet | babble*) manifest=(--manifest "$BENCH/rec/manifest.json") ;; esac
  flags=()
  for f in "$@"; do flags+=("${f//@PROG@/$prog}"); done
  "$BIN" --model "$model" --silero "$SILERO" --wake "hey goose" \
    ${variants[@]+"${variants[@]}"} ${manifest[@]+"${manifest[@]}"} ${flags[@]+"${flags[@]}"} \
    --out "$BENCH/out/$name/$prog.jsonl" "$rec" > "$BENCH/out/$name/$prog.log" 2>&1 || echo "FAILED $name $prog"
done
python3 "$B/score.py" "$BENCH/rec/manifest.json" "$BENCH"/out/"$name"/*.jsonl > "$BENCH/out/$name/score.json"
echo "$name done"
