#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Builds the end-to-end capture fixture (committed, so CI needs no sox):
#   fixtures/e2e-okay-alfred-what-time.wav = 2 s of silence + "okay alfred" (Piper lessac, from
#   crates/wakeword/fixtures/alfred) + "che ore sono" (crates/wakeword/fixtures/negatives/ora.wav)
#   + 10 s of silence; 16 kHz mono S16. The leading silence covers the wake word's 1 s cool-off
#   and lets the fake Home Assistant connect before the wake word is heard.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
ww="$here/../wakeword/fixtures"
fmt="-r 16000 -c 1 -b 16 -e signed-integer"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sox -n $fmt "$tmp/lead.wav" trim 0 2
sox -n $fmt "$tmp/tail.wav" trim 0 10
sox "$tmp/lead.wav" "$ww/alfred/lessac_okay_alfred.wav" "$ww/negatives/ora.wav" "$tmp/tail.wav" $fmt \
    "$here/fixtures/e2e-okay-alfred-what-time.wav"
soxi -D "$here/fixtures/e2e-okay-alfred-what-time.wav"
