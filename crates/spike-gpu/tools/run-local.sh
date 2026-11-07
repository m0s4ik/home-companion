#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Build-machine checks (not the device: timings here mean little):
# 1. soft-blit and soft-slint on a fake framebuffer file (800x1280, line 3200),
#    screenshot checked against the real frames and the framebuffer dump;
# 2. gpu-window (femtovg in an X window) under Xvfb with Mesa's llvmpipe, when
#    the binary was built with --features gpu-window.
# Usage: tools/run-local.sh [path/to/spike-gpu]   (frames: tools/fetch-frames.sh first)
set -euo pipefail

crate="$(cd "$(dirname "$0")/.." && pwd)"
root="$(cd "$crate/../.." && pwd)"
bin="${1:-$root/target/release/spike-gpu}"
out="$crate/out"
frames="$crate/fixtures/generated/alfred-animations"
mkdir -p "$out"
[ -d "$frames" ] || { echo "run-local: no frames, run tools/fetch-frames.sh first" >&2; exit 1; }

fake_fb() { head -c $((1280 * 3200)) /dev/zero | tr '\0' '\200' > "$out/fake-fb.bin"; }
check() { python3 -I "$crate/tools/check-shot.py" "$@" --frames-dir "$frames"; }
box() { python3 -I -c 'import json,sys; print(",".join(map(str, json.load(open(sys.argv[1]))["char_box"])))' "$1"; }

for mode in soft-blit soft-slint; do
    for scale in 1 1.3; do
        name="local-$mode-x$scale"
        fake_fb
        "$bin" --mode "$mode" --fb "$out/fake-fb.bin" --fb-geometry 800x1280x3200 --no-vt \
            --char-scale "$scale" --seconds 3.4 --pose-every 1 \
            --stats "$out/$name.json" --screenshot "$out/$name.png" > "$out/$name.txt"
        cat "$out/$name.txt"
        check "$out/$name.png" --box "$(box "$out/$name.json")" --fb "$out/fake-fb.bin"
    done
done

# Stats sanity: 16 fps, a pose switch, every tick a frame.
python3 -I - "$out/local-soft-blit-x1.json" <<'PY'
import json, sys
j = json.load(open(sys.argv[1]))
assert j["ticks_total"] >= 54, j["ticks_total"]
assert "pose" in j["classes"] and "anim" in j["classes"], j["classes"].keys()
assert abs(j["achieved_tick_hz"] - 16) < 1, j["achieved_tick_hz"]
assert j["frames_total"] >= j["ticks_total"], (j["frames_total"], j["ticks_total"])
blit = j["classes"]["anim"]["blit_ms"]
assert blit and blit["p50"] > 0, blit
print(f"stats: {j['frames_total']} frames, {j['ticks_total']} ticks, blit p50 {blit['p50']} ms: ok")
PY

if "$bin" --help | grep -q '^modes in this binary:.*gpu-window'; then
    for tex in gl upload; do
        name="local-gpu-window-$tex"
        LIBGL_ALWAYS_SOFTWARE=1 xvfb-run -a -s "-screen 0 800x1280x24" \
            "$bin" --mode gpu-window --textures "$tex" --seconds 3.4 --pose-every 1 \
            --stats "$out/$name.json" --screenshot "$out/$name.png" > "$out/$name.txt" 2> "$out/$name.err" \
            || { cat "$out/$name.err"; exit 1; }
        cat "$out/$name.txt"
        grep -E "GL|uploaded" "$out/$name.err" || true
        check "$out/$name.png" --box "$(box "$out/$name.json")" --max-diff 6
    done
fi

if "$bin" --mode soft-blit --fb /nonexistent --seconds 1 2>"$out/nonexistent.err"; then
    echo "FAIL: --fb /nonexistent exited 0" >&2
    exit 1
fi
if grep -q panicked "$out/nonexistent.err"; then
    echo "FAIL: --fb /nonexistent panicked" >&2
    exit 1
fi
echo "missing framebuffer: $(cat "$out/nonexistent.err")"
echo "run-local: all checks passed"
