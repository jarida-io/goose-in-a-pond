#!/usr/bin/env bash
# at_base_volume.sh SOURCE PASS PROGRAM...: run_pass.sh with SOURCE at its card's 0 dB (PulseAudio's
# Base Volume), then the volume found put back, whatever happens.
set -u
src=$1; shift
read_field() { LC_ALL=C pactl list sources | awk -v n="Name: $src" -v k="$1" '$0 ~ n {f=1} f && $0 ~ k {print $3; exit}'; }
found=$(read_field '^\tVolume:')
base=$(read_field '^\tBase Volume:')
[ -n "$found" ] && [ -n "$base" ] || { echo "no volume for $src"; exit 1; }
trap 'pactl set-source-volume "$src" "$found"; echo "restored $found"' EXIT
pactl set-source-volume "$src" "$base"
echo "found $found, set $base"
"$(dirname "$0")/run_pass.sh" "$@"
