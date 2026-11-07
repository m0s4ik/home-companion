#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Build the test WAVs (16 kHz mono S16) under fixtures/generated/:
#   okay_nabu_<voice>_<variant>.wav  Piper-synthesized positives, 1 s of silence before and after
#   fuori1..3.wav, ora.wav           Italian negatives copied from $PROVE_VOSK/wav
#   silence.wav                      5 s of digital silence
# Needs the Piper virtualenv of $PROVE_VOSK (default ~/prove-vosk) and sox; without sox,
# tools/resample16k.py (linear interpolation, lower quality) is used instead.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
prove=${PROVE_VOSK:-$HOME/prove-vosk}
py="$prove/venv/bin/python"
out="$here/fixtures/generated"
voices="$out/voices"
tmp="$out/tmp"
mkdir -p "$voices" "$tmp"

if [ ! -x "$py" ]; then
    echo "error: Piper virtualenv not found at $prove/venv (set PROVE_VOSK)" >&2
    exit 1
fi

# to16k <in.wav> <out.wav>: 16 kHz mono S16 with 1 s of silence before and after.
to16k() {
    if command -v sox >/dev/null; then
        sox -q -G "$1" -r 16000 -c 1 -b 16 -e signed-integer "$2" pad 1 1
    else
        python3 -I "$here/tools/resample16k.py" "$1" "$2" 1
    fi
}

download_voice() {
    [ -f "$voices/$1.onnx" ] && return 0
    (cd "$voices" && "$py" -m piper.download_voices "$1" --download-dir "$voices")
}

# synth <voice> <name> <length_scale> <text>
synth() {
    local voice=$1 name=$2 ls=$3 text=$4 short dst
    short=${voice#*-}
    dst="$out/okay_nabu_${short%-*}_$name.wav"
    printf '%s\n' "$text" | (cd "$tmp" && "$py" -m piper -m "$voices/$voice.onnx" \
        --length-scale "$ls" -f "$tmp/raw.wav" 2>/dev/null)
    to16k "$tmp/raw.wav" "$dst"
    echo "made $(basename "$dst")"
}

download_voice en_US-lessac-medium
synth en_US-lessac-medium plain 1.0 "okay nabu"
synth en_US-lessac-medium comma 1.0 "okay, nabu"
synth en_US-lessac-medium fast 0.9 "okay nabu"
synth en_US-lessac-medium slow 1.2 "okay nabu"
synth en_US-lessac-medium comma_slow 1.2 "Okay, Nabu."

if download_voice en_US-amy-medium; then
    synth en_US-amy-medium plain 1.0 "okay nabu"
    synth en_US-amy-medium comma 1.0 "okay, nabu"
    synth en_US-amy-medium slow 1.2 "okay nabu"
else
    echo "warning: en_US-amy-medium not available, only one voice" >&2
fi

for f in fuori1 fuori2 fuori3 ora; do
    cp "$prove/wav/$f.wav" "$out/$f.wav"
done
# The 12 Italian sentences and their transcripts, for run-vosk-tests.sh (copied so that the
# device needs only fixtures/generated/).
mkdir -p "$out/vosk"
cp "$prove"/wav/*.wav "$prove/frasi.txt" "$out/vosk/"
if command -v sox >/dev/null; then
    sox -q -n -r 16000 -c 1 -b 16 -e signed-integer "$out/silence.wav" trim 0 5
else
    python3 -I "$here/tools/resample16k.py" --silence "$out/silence.wav" 5
fi
rm -rf "$tmp"

# Every file must be 16 kHz mono S16.
python3 -I -c '
import os, sys, wave
bad = 0
for p in sys.argv[1:]:
    with wave.open(p) as w:
        ok = (w.getframerate(), w.getnchannels(), w.getsampwidth()) == (16000, 1, 2)
        secs = w.getnframes() / w.getframerate()
        print("%-32s %5.2f s %s" % (os.path.basename(p), secs, "ok" if ok else "WRONG FORMAT"))
        bad += not ok
sys.exit(bad)
' "$out"/*.wav
