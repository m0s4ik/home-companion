#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
"""Fallback for make-wavs.sh when sox is not available: convert a 16-bit WAV to 16 kHz mono S16
with plain linear interpolation (no anti-alias filter, so lower quality than sox), optionally
padding silence before and after.

usage: resample16k.py IN.wav OUT.wav [PAD_SECONDS]
       resample16k.py --silence OUT.wav SECONDS
"""
import array
import sys
import wave

RATE = 16000


def write(path, samples):
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(samples.tobytes())


def main(argv):
    if len(argv) == 3 and argv[0] == "--silence":
        write(argv[1], array.array("h", [0] * int(float(argv[2]) * RATE)))
        return
    src, dst = argv[0], argv[1]
    pad = float(argv[2]) if len(argv) > 2 else 0.0
    with wave.open(src, "rb") as w:
        if w.getsampwidth() != 2:
            sys.exit(f"{src}: only 16-bit input is supported")
        ch, rate = w.getnchannels(), w.getframerate()
        data = array.array("h", w.readframes(w.getnframes()))
    mono = [sum(data[i:i + ch]) / ch for i in range(0, len(data), ch)]
    n_out = int(len(mono) * RATE / rate)
    out = array.array("h")
    for i in range(n_out):
        x = i * rate / RATE
        j = int(x)
        frac = x - j
        a = mono[j]
        b = mono[j + 1] if j + 1 < len(mono) else a
        out.append(max(-32768, min(32767, round(a + (b - a) * frac))))
    silence = array.array("h", [0] * int(pad * RATE))
    write(dst, silence + out + silence)


if __name__ == "__main__":
    main(sys.argv[1:])
