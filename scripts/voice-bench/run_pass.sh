#!/usr/bin/env bash
# run_pass.sh PASS PROGRAM...: plays each program (or 300 s of nothing for "silence") while every
# microphone in MICS records at 16 kHz, into $BENCH/rec/PASS-PROGRAM-NAME.wav.
set -u
BENCH=${BENCH:-$HOME/voice-bench-frontend}
# name=source pairs; the defaults are the household nano's USB mic and its C920.
MICS=${MICS:-usb=alsa_input.usb-Solid_State_System_Co._Ltd._USB_PnP_Audio_Device_000000000000-00.analog-stereo cam=alsa_input.usb-046d_HD_Pro_Webcam_C920_909474DF-02.analog-stereo}
pass=$1; shift
mkdir -p "$BENCH/rec"
for prog in "$@"; do
  pids=()
  for pair in $MICS; do
    parec -d "${pair#*=}" --rate=16000 --channels=1 --format=s16le --file-format=wav \
      "$BENCH/rec/$pass-$prog-${pair%%=*}.wav" & pids+=($!)
  done
  sleep 2
  echo "$(date -u +%T) $pass $prog start"
  if [ "$prog" = silence ]; then sleep 300; else paplay "$BENCH/programs/$prog.wav"; fi
  sleep 2
  kill -INT "${pids[@]}"; wait "${pids[@]}" 2>/dev/null
  echo "$(date -u +%T) $pass $prog done"
done
