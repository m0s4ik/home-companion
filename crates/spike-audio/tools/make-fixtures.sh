#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Generates crates/spike-audio/fixtures/generated/answer-22050.wav (22050 Hz, mono, S16):
# an Italian two-sentence answer spoken by Piper when it is available, otherwise a 3 s
# vowel-like tone written with python3's `wave` module. Nothing generated is committed.
#
# Also writes fixtures/generated/echo-16000.wav (16000 Hz, 2 ch, S16): the same answer as a
# microphone would hear it from the speaker (downsampled, 120 ms late, -6 dB, plus a little
# noise), 10 s long. It feeds `duplex --backend file --in-wav` to check echo cancellation
# here without any sound hardware.
#
# Environment (optional):
#   PIPER        piper executable      (default: ~/prove-vosk/venv/bin/piper)
#   PIPER_MODEL  voice model (.onnx)   (default: ~/prove-vosk/it_IT-paola-medium.onnx)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out_dir="$here/fixtures/generated"
out="$out_dir/answer-22050.wav"
piper="${PIPER:-$HOME/prove-vosk/venv/bin/piper}"
model="${PIPER_MODEL:-$HOME/prove-vosk/it_IT-paola-medium.onnx}"
text="Sono le dieci e un quarto. Domani il cielo sarà sereno con una massima di ventidue gradi."

mkdir -p "$out_dir"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

made=""
if [[ -x "$piper" && -f "$model" ]]; then
    if echo "$text" | "$piper" -m "$model" -f "$tmp/piper.wav" >"$tmp/piper.log" 2>&1; then
        made="$tmp/piper.wav"
        echo "spoken by Piper ($(basename "$model"))"
    else
        echo "Piper failed, using the synthetic fallback:" >&2
        tail -n 5 "$tmp/piper.log" >&2
    fi
else
    echo "Piper or its model not found, using the synthetic fallback" >&2
fi

# Normalise to 22050 Hz mono S16 (Piper medium voices already are) or synthesize.
python3 -I - "$made" "$out" <<'EOF'
import math, sys, wave

src, dst = sys.argv[1], sys.argv[2]
if src:
    with wave.open(src, "rb") as w:
        rate, ch, width, n = w.getframerate(), w.getnchannels(), w.getsampwidth(), w.getnframes()
        data = w.readframes(n)
    if (rate, ch, width) != (22050, 1, 2):
        sys.exit(f"unexpected Piper output: {rate} Hz, {ch} ch, {8 * width} bit")
else:
    # 3 s "aah": 140 Hz glottal-like harmonics shaped by two formants, with a slow envelope.
    rate, n = 22050, 3 * 22050
    out = bytearray()
    for i in range(n):
        t = i / rate
        s = 0.0
        for k in range(1, 30):
            f = 140.0 * k
            amp = math.exp(-((f - 700.0) / 180.0) ** 2) + 0.5 * math.exp(-((f - 1200.0) / 200.0) ** 2) + 0.05 / k
            s += amp * math.sin(2 * math.pi * f * t)
        env = min(1.0, t / 0.05, (3.0 - t) / 0.05) * (0.8 + 0.2 * math.sin(2 * math.pi * 3 * t))
        v = int(max(-32767, min(32767, 6000.0 * env * s)))
        out += v.to_bytes(2, "little", signed=True)
    data = bytes(out)
with wave.open(dst, "wb") as w:
    w.setnchannels(1)
    w.setsampwidth(2)
    w.setframerate(22050)
    w.writeframes(data)
with wave.open(dst, "rb") as w:
    print(f"{dst}: {w.getframerate()} Hz, {w.getnchannels()} ch, {8 * w.getsampwidth()} bit, "
          f"{w.getnframes() / w.getframerate():.2f} s")
EOF

python3 -I - "$out" "$out_dir/echo-16000.wav" <<'EOF'
import random, sys, wave

src, dst = sys.argv[1], sys.argv[2]
with wave.open(src, "rb") as w:
    rate = w.getframerate()
    raw = w.readframes(w.getnframes())
x = [int.from_bytes(raw[i:i + 2], "little", signed=True) for i in range(0, len(raw), 2)]
out_rate, seconds, delay_s, gain = 16000, 10.0, 0.120, 0.5
random.seed(1)
frames = bytearray()
for i in range(int(out_rate * seconds)):
    t = i / out_rate - delay_s
    v = 0.0
    if t >= 0:
        # Linear interpolation is enough for a test echo.
        pos = t * rate
        k = int(pos)
        if k + 1 < len(x):
            v = x[k] + (x[k + 1] - x[k]) * (pos - k)
    s = int(max(-32767, min(32767, gain * v + random.gauss(0.0, 30.0))))
    frames += s.to_bytes(2, "little", signed=True) * 2
with wave.open(dst, "wb") as w:
    w.setnchannels(2)
    w.setsampwidth(2)
    w.setframerate(out_rate)
    w.writeframes(bytes(frames))
print(f"{dst}: {out_rate} Hz, 2 ch, 16 bit, {seconds:.2f} s (echo of the answer)")
EOF
