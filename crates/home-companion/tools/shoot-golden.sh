#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Renders the golden screenshots: the simulator window under Xvfb at scale 1, every state with
# the fixed sample data of `--state`, into OUT_DIR (default crates/home-companion/out/golden).
# usage: shoot-golden.sh [OUT_DIR] [TARGET_DIR]
set -eu
root=$(cd "$(dirname "$0")/../../.." && pwd)
out=${1:-$root/crates/home-companion/out/golden}
bin=${2:-$root/target/release}
mkdir -p "$out"
cd "$root"
export TZ=UTC
for s in home cards listening thinking speaking error offline ringing volume altro chat timers settings language screen music setup night starting suspend new-timer new-alarm; do
    SLINT_SCALE_FACTOR=1 timeout 60 xvfb-run -a -s "-screen 0 1280x800x24" \
        "$bin/home-companion" --simulator --config crates/home-companion/ci/sim.toml --state "$s" \
        --screenshot "$out/$s.png" --exit-after 2 >"$out/$s.log" 2>&1 || {
        echo "shoot-golden: $s failed"; cat "$out/$s.log"; exit 1; }
    echo "shoot-golden: $out/$s.png"
done
