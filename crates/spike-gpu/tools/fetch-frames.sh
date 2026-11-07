#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Gets the Alfred animation frames (CC0) and extracts them into the crate's
# git-ignored fixtures: fixtures/generated/alfred-animations/<pose>/frame_NNN.png.
#
# Usage: tools/fetch-frames.sh [ZIP]
#   ZIP: a local alfred-animations-v0.1.zip. Without it the zip is downloaded
#   from the thinksmart-view-ha-companion release v0.1 with `gh` (the release
#   is still a private draft: needs `gh auth login` with access to it).
# Prints the pose folders with their frame count and frame size.
set -euo pipefail

crate="$(cd "$(dirname "$0")/.." && pwd)"
dest="$crate/fixtures/generated"
name="alfred-animations-v0.1.zip"
mkdir -p "$dest"

zip="${1:-}"
if [ -z "$zip" ]; then
    zip="$dest/$name"
    if [ ! -f "$zip" ]; then
        gh release download v0.1 --repo m0s4ik/thinksmart-view-ha-companion \
            --pattern "$name" --pattern "$name.sha256" --dir "$dest"
        (cd "$dest" && sha256sum -c "$name.sha256")
    fi
fi
[ -f "$zip" ] || { echo "fetch-frames: no such file: $zip" >&2; exit 1; }

# Extract into an empty folder first, then move into place.
tmp="$(mktemp -d "$dest/extract.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
unzip -q "$zip" -d "$tmp"
[ -d "$tmp/alfred-animations" ] || { echo "fetch-frames: $zip has no alfred-animations/ folder" >&2; exit 1; }
rm -rf "$dest/alfred-animations"
mv "$tmp/alfred-animations" "$dest/alfred-animations"

for d in "$dest"/alfred-animations/*/; do
    n=$(find "$d" -maxdepth 1 -name 'frame_*.png' | wc -l)
    first=$(find "$d" -maxdepth 1 -name 'frame_*.png' | sort | head -n1)
    # PNG IHDR: width and height are big-endian at bytes 16..23.
    size=$(od -An -tu1 -j16 -N8 "$first" | awk '{printf "%dx%d", $1*16777216+$2*65536+$3*256+$4, $5*16777216+$6*65536+$7*256+$8}')
    echo "$(basename "$d"): $n frames, $size"
done
echo "frames in $dest/alfred-animations"
