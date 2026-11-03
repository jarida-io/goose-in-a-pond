#!/usr/bin/env bash
# two_speaker_pass.sh HOST PASS NAME=PROGRAM[+BED@VOLUME]...: run_pass.sh on HOST, its speaker
# playing PROGRAM, while this machine's speakers play BED (afplay at VOLUME) from the moment each
# program starts, so the babble comes from another place in the room than the talkers.
set -u
host=$1 pass=$2; shift 2
REMOTE=${REMOTE:-voice-bench-bf}
for arg in "$@"; do
  name=${arg%%=*} rest=${arg#*=}
  prog=${rest%%+*} bed= vol=1
  case $rest in *+*) bed=${rest#*+}; vol=${bed##*@}; bed=${bed%@*} ;; esac
  player=
  while IFS= read -r line; do
    echo "$line"
    if [ -n "$bed" ] && [[ $line == *" $pass $name start" ]]; then
      afplay -v "$vol" "$bed" & player=$!
    fi
  done < <(ssh "$host" "BENCH=\$HOME/$REMOTE MICS='${MICS:-}' bash \$HOME/$REMOTE/run_pass.sh $pass $name=$prog")
  if [ -n "$player" ]; then kill "$player" 2>/dev/null; wait "$player" 2>/dev/null; fi
done
