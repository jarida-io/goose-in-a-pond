#!/usr/bin/env bash
# run_pass.sh PASS PROGRAM...: plays each program (or 300 s of nothing for "silence") while every
# microphone in MICS records at 16 kHz, into $BENCH/rec/PASS-NAME-MIC.wav. A program may be
# NAME=FILE (programs/FILE.wav, or silence:SECONDS) to record under another name.
set -u
BENCH=${BENCH:-$HOME/voice-bench-frontend}
# name=source[@channels]; the defaults are the household nano's USB mic and its C920.
MICS=${MICS:-usb=alsa_input.usb-Solid_State_System_Co._Ltd._USB_PnP_Audio_Device_000000000000-00.analog-stereo cam=alsa_input.usb-046d_HD_Pro_Webcam_C920_909474DF-02.analog-stereo}
pass=$1; shift
mkdir -p "$BENCH/rec"
for arg in "$@"; do
  name=${arg%%=*}
  prog=${arg#*=}
  pids=()
  for pair in $MICS; do
    mic=${pair%%=*} src=${pair#*=} ch=1
    case $src in *@*) ch=${src##*@} src=${src%@*} ;; esac
    parec -d "$src" --rate=16000 --channels="$ch" --format=s16le --file-format=wav \
      "$BENCH/rec/$pass-$name-$mic.wav" & pids+=($!)
  done
  sleep 2
  echo "$(date -u +%T) $pass $name start"
  case $prog in
    silence) sleep 300 ;;
    silence:*) sleep "${prog#silence:}" ;;
    *) paplay "$BENCH/programs/$prog.wav" ;;
  esac
  sleep 2
  kill -INT "${pids[@]}"; wait "${pids[@]}" 2>/dev/null
  echo "$(date -u +%T) $pass $name done"
done
