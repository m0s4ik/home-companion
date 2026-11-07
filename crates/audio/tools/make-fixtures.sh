#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Regenerates the small committed fixtures of crates/audio/fixtures/ with sox (repeatable:
# `sox -R`). Nothing here is a recording of a person.
#
#   answer-22050.wav     22050 Hz mono S16, 1.5 s: 440 Hz then 554 Hz, -12 dBFS peak, 10 ms fades
#                        (the "answer" that audio-bench play --cues streams like Home Assistant)
#   speech-16k.wav       16000 Hz mono S16, 3 s: five 0.4 s bursts of 300-3400 Hz noise at
#                        -20 dBFS RMS separated by 0.2 s of silence (AGC CPU measurement)
#   stereo-left-16k.wav  16000 Hz stereo S16, 1 s: 1 kHz at -6 dBFS peak on the left, right silent
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$here/fixtures"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$out"
s() { sox -R -V1 "$@"; }

s -n -r 22050 -c 1 -b 16 "$tmp/a.wav" synth 0.75 sine 440 fade t 0.01 0.75 0.01 vol 0.25
s -n -r 22050 -c 1 -b 16 "$tmp/b.wav" synth 0.75 sine 554 fade t 0.01 0.75 0.01 vol 0.25
s "$tmp/a.wav" "$tmp/b.wav" "$out/answer-22050.wav"

s -n -r 16000 -c 1 -b 16 "$tmp/burst.wav" synth 0.4 whitenoise sinc 300-3400 fade t 0.01 0.4 0.01
rms="$(sox "$tmp/burst.wav" -n stats 2>&1 | awk '/^RMS lev dB/ {print $4}')"
gain="$(awk -v r="$rms" 'BEGIN { printf "%.2f", -20 - r }')"
s "$tmp/burst.wav" "$tmp/burst20.wav" gain "$gain"
s -n -r 16000 -c 1 -b 16 "$tmp/gap.wav" trim 0 0.2
s "$tmp/burst20.wav" "$tmp/gap.wav" "$tmp/burst20.wav" "$tmp/gap.wav" "$tmp/burst20.wav" \
  "$tmp/gap.wav" "$tmp/burst20.wav" "$tmp/gap.wav" "$tmp/burst20.wav" "$tmp/gap.wav" \
  "$out/speech-16k.wav"

s -n -r 16000 -c 1 -b 16 "$tmp/tone.wav" synth 1 sine 1000 vol 0.5
s "$tmp/tone.wav" -c 2 "$out/stereo-left-16k.wav" remix 1 0

for f in "$out"/*.wav; do
    printf '%s: %s\n' "$(basename "$f")" "$(soxi "$f" | awk -F': ' '/Sample Rate|Channels|Duration/ {printf "%s ", $2}')"
done
