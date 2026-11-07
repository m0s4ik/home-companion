#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Headless render on this machine: 48 frames with a pose swap and a volume step
# every second, screenshot + stats, then the acceptance checks. Then the same
# through the fbdev code path on a fake framebuffer file (both render modes).
# Usage: tools/run-headless.sh [path/to/spike-ui]
set -euo pipefail

crate="$(cd "$(dirname "$0")/.." && pwd)"
root="$(cd "$crate/../.." && pwd)"
bin="${1:-$root/target/release/spike-ui}"
out="$crate/out"
mkdir -p "$out"

"$bin" --backend headless --frames 48 --pose-swap-every 1 --volume-step-every 1 \
    --screenshot "$out/headless.png" --stats "$out/headless.json"
python3 -I "$crate/tools/check-png.py" "$out/headless.png"

python3 -I - "$out/headless.json" <<'EOF'
import json, sys
j = json.load(open(sys.argv[1]))
frames = j["frames"]
assert len(frames) >= 48, f"only {len(frames)} frames"
pose = [f for f in frames if f["class"] == "pose" and f["dirty_px"] >= 600 * 600]
assert pose, "no pose frame with >= 600x600 dirty pixels"
box = [f["dirty_px"] for f in frames if f["class"] == "box"]
assert box and max(box) <= 4 * 64 * 64, f"box frames dirty px max {max(box)}"
print(f"stats: {len(frames)} frames, {len(pose)} pose frames, box dirty px max {max(box)}: ok")
EOF

for mode in fb ram; do
    "$crate/tools/make-fake-fb.sh" "$out/fake-fb.bin" >/dev/null
    "$bin" --backend fbdev --fb "$out/fake-fb.bin" --fb-geometry 800x1280x3200 --no-vt \
        --render-into "$mode" --frames 40 --pose-swap-every 1 \
        --screenshot "$out/fakefb-$mode.png" --stats "$out/fakefb-$mode.json" >/dev/null
    python3 -I "$crate/tools/check-png.py" "$out/fakefb-$mode.png" --fb "$out/fake-fb.bin" --geometry 800x1280x3200
done

if "$bin" --backend fbdev --fb /nonexistent 2>"$out/nonexistent.err"; then
    echo "FAIL: --fb /nonexistent exited 0" >&2
    exit 1
fi
if grep -q panicked "$out/nonexistent.err"; then
    echo "FAIL: --fb /nonexistent panicked" >&2
    exit 1
fi
echo "missing framebuffer: $(cat "$out/nonexistent.err")"
echo "run-headless: all checks passed"
