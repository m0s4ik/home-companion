#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Regenerates the Inter subsets used by ui/alfred.slint (Light, Regular,
# Medium, SemiBold) from the static TTFs of Inter 4.1 (rsms/inter release zip,
# extras/ttf/, SIL Open Font License 1.1) and copies the license next to them.
# Same ranges as make-font.sh plus the degree sign and dashes; each subset is
# ~30 KB instead of ~410 KB. Needs fontTools (python3 -m fontTools.subset).
#
#   tools/make-font-inter.sh DIR_WITH_Inter-*.ttf [LICENSE.txt]
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
src="${1:?directory with Inter-Light.ttf, Inter-Regular.ttf, Inter-Medium.ttf, Inter-SemiBold.ttf}"
license="${2:-$src/LICENSE.txt}"
dest="$here/fixtures/fonts"
mkdir -p "$dest"

for weight in Light Regular Medium SemiBold; do
    python3 -m fontTools.subset "$src/Inter-$weight.ttf" \
        --unicodes="U+0020-007E,U+00A0-00FF,U+2013-2014,U+2022,U+2026,U+25CF" \
        --layout-features='kern,liga,tnum' \
        --no-hinting \
        --name-IDs='*' \
        --output-file="$dest/Inter-$weight-subset.ttf"
done

cp "$license" "$dest/Inter-LICENSE.txt"
ls -l "$dest"
