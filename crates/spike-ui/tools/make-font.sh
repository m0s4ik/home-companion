#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Regenerates fixtures/fonts/DejaVuSans-subset.ttf from the system DejaVu Sans
# (Debian package fonts-dejavu-core) and copies its license next to it.
# The full font is 760 KB; the subset keeps Latin-1 (U+0020-U+00FF) plus a few
# symbols, which is all the spike draws, and stays under the per-crate fixture
# budget. Slint only reads the font at build time (EmbedForSoftwareRenderer
# pre-renders the glyphs into the binary).
# Needs python3-fonttools.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
src="${1:-/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf}"
dest="$here/fixtures/fonts"
mkdir -p "$dest"

python3 -m fontTools.subset "$src" \
    --unicodes="U+0020-007E,U+00A0-00FF,U+2022,U+2026,U+25CF" \
    --layout-features='kern,liga' \
    --no-hinting \
    --name-IDs='*' \
    --output-file="$dest/DejaVuSans-subset.ttf"

cp /usr/share/doc/fonts-dejavu-core/copyright "$dest/DejaVuSans-LICENSE.txt"
ls -l "$dest"
