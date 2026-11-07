#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Run mww over the generated positives and negatives and print a table.
# Fails (exit 1) unless at least one "okay nabu" variant is detected and no negative is.
# Also builds a ~60 s file (positives + negatives + silence, concatenated) for the CPU figure.
#
# Environment:
#   MWW         the binary (default: ../../target/release/mww from the repo, else bin/mww here)
#   RUNNER      a prefix for the binary, e.g. "qemu-aarch64 -L /usr/aarch64-linux-gnu" or "nice -n 10 taskset -c 3"
#   CUTOFF      override the manifest cutoff (diagnosis only)
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
gen="$here/fixtures/generated"
mww=${MWW:-}
if [ -z "$mww" ]; then
    if [ -x "$here/../../target/release/mww" ]; then mww="$here/../../target/release/mww"; else mww="$here/bin/mww"; fi
fi
read -r -a runner <<<"${RUNNER:-}"
model=(--model "$here/models/okay_nabu.tflite" --manifest "$here/models/v2/okay_nabu.json")
if [ -n "${CUTOFF:-}" ]; then model+=(--cutoff "$CUTOFF"); fi

shopt -s nullglob
positives=("$gen"/okay_nabu_*.wav)
negatives=("$gen/silence.wav" "$gen/fuori1.wav" "$gen/fuori2.wav" "$gen/fuori3.wav" "$gen/ora.wav")
if [ ${#positives[@]} -eq 0 ]; then
    echo "error: no positives in $gen (run tools/make-wavs.sh)" >&2
    exit 2
fi

# ~60 s: every test file in turn, repeated until 60 s.
python3 -I - "$gen/concat60.wav" "${positives[@]}" "${negatives[@]}" <<'EOF'
import sys, wave
out, files = sys.argv[1], sys.argv[2:]
frames, total = [], 0
while total < 60 * 16000:
    for f in files:
        if total >= 60 * 16000:
            break
        with wave.open(f) as w:
            frames.append(w.readframes(w.getnframes()))
            total += w.getnframes()
with wave.open(out, "wb") as w:
    w.setnchannels(1); w.setsampwidth(2); w.setframerate(16000)
    w.writeframes(b"".join(frames))
EOF

results=$(mktemp)
trap 'rm -f "$results"' EXIT
run() { # run <kind> <wav>
    local line rc=0
    line=$("${runner[@]}" "$mww" "${model[@]}" --wav "$2" --quiet) || rc=$?
    if [ "$rc" -ge 2 ]; then
        echo "error: mww failed on $2" >&2
        exit 2
    fi
    printf '%s %s\n' "$1" "${line#RESULT }" >>"$results"
}
for f in "${positives[@]}"; do run positive "$f"; done
for f in "${negatives[@]}"; do run negative "$f"; done
run cpu "$gen/concat60.wav"

python3 -I - "$results" <<'EOF'
import json, os, sys
rows = [(l.split(" ", 1)[0], json.loads(l.split(" ", 1)[1])) for l in open(sys.argv[1])]
print(f"{'file':34s} {'kind':8s} {'detected':8s} {'at (s)':12s} {'max prob':8s} {'cpu/s audio':11s}")
for kind, r in rows:
    at = ",".join(f"{t:.2f}" for t in r["detections_s"]) or "-"
    print(f"{os.path.basename(r['wav']):34s} {kind:8s} {'yes' if r['detected'] else 'no':8s} {at:12s} "
          f"{r['max_prob_f']:8.3f} {r['cpu_per_audio_second']:11.5f}")
cpu = [r for k, r in rows if k == "cpu"][0]
print(f"\n60 s file: audio {cpu['audio_s']:.1f} s, cpu {cpu['cpu_s']:.3f} s, cpu_per_audio_second "
      f"{cpu['cpu_per_audio_second']:.5f}, init cpu {cpu['cpu_init_s']:.4f} s, peak RSS {cpu['peak_rss_mb']:.1f} MB, "
      f"arena {cpu['arena_used']}/{cpu['arena_size']} bytes, stride {cpu['stride']}, cutoff {cpu['cutoff']}, "
      f"window {cpu['window']}")
pos = [r for k, r in rows if k == "positive"]
neg = [r for k, r in rows if k == "negative"]
hits = sum(r["detected"] for r in pos)
false = [os.path.basename(r["wav"]) for r in neg if r["detected"]]
print(f"positives detected: {hits}/{len(pos)}; negatives detected: {len(false)}/{len(neg)} {false or ''}")
ok = hits >= 1 and not false
print("PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
EOF
