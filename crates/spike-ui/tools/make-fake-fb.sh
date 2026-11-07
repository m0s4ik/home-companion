#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Creates a fake 800x1280 32 bpp framebuffer: a plain file of 1280 * 3200
# bytes (the device's line length), filled with a grey pattern so that a
# region the program never wrote is easy to spot. Use it with
#   spike-ui --backend fbdev --fb FILE --fb-geometry 800x1280x3200 --no-vt
set -euo pipefail

out="${1:-$(cd "$(dirname "$0")/.." && pwd)/out/fake-fb.bin}"
mkdir -p "$(dirname "$out")"
# 0x80 everywhere: mid grey, X byte 0x80.
head -c $((1280 * 3200)) /dev/zero | tr '\0' '\200' > "$out"
echo "$out"
