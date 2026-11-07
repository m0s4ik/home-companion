#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Regenerates the WAV fixtures of the fake Home Assistant (committed, so CI needs no sox):
#   answer-22050.wav   1.5 s, 22050 Hz mono S16: 660/880/1100 Hz tones of 300 ms, 100 ms gaps,
#                      peak -10 dBFS, silence to 1.5 s (what Piper-like TTS sends to a satellite)
#   announce-22050.wav 0.8 s, 22050 Hz mono S16: 523/784 Hz tones of 300 ms, 100 ms gap, -10 dBFS
set -eu
cd "$(dirname "$0")/../fixtures"
fmt="-r 22050 -c 1 -b 16 -e signed-integer"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
tone() { sox -n $fmt "$tmp/$1.wav" synth 0.3 sine "$2" fade t 0.01 0.3 0.01; }
gap() { sox -n $fmt "$tmp/$1.wav" trim 0 0.1; }
tone a 660; gap g1; tone b 880; gap g2; tone c 1100
sox "$tmp/a.wav" "$tmp/g1.wav" "$tmp/b.wav" "$tmp/g2.wav" "$tmp/c.wav" $fmt answer-22050.wav gain -n -10 pad 0 0.4
tone d 523; tone e 784
sox "$tmp/d.wav" "$tmp/g1.wav" "$tmp/e.wav" $fmt announce-22050.wav gain -n -10 pad 0 0.1
soxi -D answer-22050.wav announce-22050.wav
