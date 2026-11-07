#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Runs the simulator (winit backend, software renderer) under Xvfb for 3 s,
# with a screenshot at exit; if xdotool is installed, also presses S after 2 s
# to exercise the key screenshot. Needs xvfb and libxkbcommon-x11-0.
# Usage: tools/run-xvfb.sh [path/to/spike-ui]
set -euo pipefail

crate="$(cd "$(dirname "$0")/.." && pwd)"
root="$(cd "$crate/../.." && pwd)"
bin="${1:-$root/target/release/spike-ui}"
out="$crate/out"
mkdir -p "$out"
rm -f "$out/winit.png" "$out/winit-key1.png"

# The screen must be at least as tall as the window (800x1280).
inner=$(cat <<EOF
"$bin" --backend winit --seconds 3 --screenshot "$out/winit.png" --stats "$out/winit.json" &
pid=\$!
if command -v xdotool >/dev/null; then
    sleep 2
    xdotool search --sync --name spike-ui windowfocus --sync key s || true
fi
wait \$pid
EOF
)
timeout 60 xvfb-run -a -s "-screen 0 1024x1400x24" bash -c "$inner"
python3 -I "$crate/tools/check-png.py" "$out/winit.png"
if [ -f "$out/winit-key1.png" ]; then
    python3 -I "$crate/tools/check-png.py" "$out/winit-key1.png"
    echo "run-xvfb: key S screenshot ok"
elif command -v xdotool >/dev/null; then
    echo "run-xvfb: warning: key S produced no screenshot" >&2
fi
echo "run-xvfb: ok"
