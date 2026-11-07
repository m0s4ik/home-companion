#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# The synthetic reflex test set: every line of phrases-it.tsv (`id<TAB>text`;
# ids p* are commands, n* must go to the brain) said by Piper at two speeds, 16 kHz mono, -14 dB
# (the device's microphone is low), 0.3 s of silence before and 1.2 s after; plus eval.tsv for
# `home-companion-reflex --eval`.
#
#   make-testset.sh OUT_DIR PIPER VOICE.onnx      (needs sox)
set -euo pipefail
out=${1:?output directory}
piper=${2:?piper binary}
voice=${3:?piper voice .onnx}
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out/wav"
tmp=$(mktemp --suffix .wav)
trap 'rm -f "$tmp"' EXIT
: > "$out/eval.tsv"
while IFS=$'\t' read -r id text; do
    [ -n "$id" ] || continue
    for ls in 0.9 1.1; do
        f=wav/$id-$ls.wav
        if [ ! -f "$out/$f" ]; then
            echo "$text" | "$piper" -m "$voice" --length_scale "$ls" -f "$tmp" > /dev/null 2>&1
            sox "$tmp" -r 16000 -c 1 -b 16 "$out/$f" vol -14dB pad 0.3 1.2
        fi
        case $id in p*) want=$text ;; *) want=- ;; esac
        printf '%s\t%s\n' "$f" "$want" >> "$out/eval.tsv"
    done
done < "$here/phrases-it.tsv"
echo "$(wc -l < "$out/eval.tsv") files in $out; run:"
echo "  home-companion-reflex --model MODEL_DIR --grammar characters/alfred/grammar/it.toml --eval $out/eval.tsv"
