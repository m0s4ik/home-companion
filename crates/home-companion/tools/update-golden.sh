#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Regenerates crates/home-companion/fixtures/golden/*.png (after a change of companion.slint, a character
# or a locale), then checks them. Needs a release build: cargo build --release -p home-companion
set -eu
root=$(cd "$(dirname "$0")/../../.." && pwd)
tmp="$root/crates/home-companion/out/golden-new"
rm -rf "$tmp"
"$root/crates/home-companion/tools/shoot-golden.sh" "$tmp" "${1:-$root/target/release}"
cp "$tmp"/*.png "$root/crates/home-companion/fixtures/golden/"
python3 -I "$root/crates/home-companion/tools/check-golden.py" "$root/crates/home-companion/fixtures/golden" "$tmp"
