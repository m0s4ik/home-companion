#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
"""Checks a mix written by `spike-audio play --backend file`: 48000 Hz S16, and an 880 Hz
beep starting at each expected time (within the tolerance), found with a matched filter
so that an answer playing underneath does not fool it. Pure standard library.

usage: python3 -I check-mix.py mix.wav 1.0 2.5 [--rate 48000] [--tol-ms 20]

For each beep it also prints its level (dBFS of the fitted 880 Hz tone) and its duration (10 ms
windows whose 880 Hz power is within 10 dB of the strongest one): on a microphone capture of
`--gap-test` this tells a beep that is present (~200 ms), shortened (cut start) or missing (level
near the noise floor, duration erratic).
"""
import argparse
import math
import sys
import wave


def goertzel(samples, rate, freq):
    k = 2.0 * math.cos(2.0 * math.pi * freq / rate)
    s1 = s2 = 0.0
    for x in samples:
        s1, s2 = x + k * s1 - s2, s1
    return s1 * s1 + s2 * s2 - k * s1 * s2


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("wav")
    ap.add_argument("times", nargs="+", type=float)
    ap.add_argument("--rate", type=int, default=48000)
    ap.add_argument("--tol-ms", type=float, default=20.0)
    a = ap.parse_args()

    with wave.open(a.wav, "rb") as w:
        rate, ch, width, n = w.getframerate(), w.getnchannels(), w.getsampwidth(), w.getnframes()
        raw = w.readframes(n)
    print(f"{a.wav}: {rate} Hz, {ch} ch, {8 * width} bit, {n / rate:.2f} s")
    ok = rate == a.rate and width == 2
    if not ok:
        print(f"FAIL: expected {a.rate} Hz S16")
    left = [int.from_bytes(raw[i:i + 2], "little", signed=True) for i in range(0, len(raw), 2 * ch)]

    # Matched filter: quadrature correlation with the 200 ms 880 Hz beep (10 ms fades) every
    # 1 ms around each expected time. Speech overlapping the beep barely correlates with a
    # 200 ms pure tone, so the magnitude peaks where the beep starts.
    n_t, fade = rate // 5, rate // 100
    env = [min(1.0, min(i, n_t - 1 - i) / fade) for i in range(n_t)]
    tc = [e * math.cos(2 * math.pi * 880.0 * i / rate) for i, e in enumerate(env)]
    ts = [e * math.sin(2 * math.pi * 880.0 * i / rate) for i, e in enumerate(env)]
    for t in a.times:
        span = max(0.3, a.tol_ms / 1000.0)
        lo = max(0, int((t - span) * rate))
        hi = min(len(left) - n_t, int((t + span) * rate))
        best, onset = -1.0, lo
        for i in range(lo, hi, rate // 1000):
            seg = left[i:i + n_t]
            c = sum(x * y for x, y in zip(seg, tc))
            q = sum(x * y for x, y in zip(seg, ts))
            if c * c + q * q > best:
                best, onset = c * c + q * q, i
        err_ms = (onset / rate - t) * 1000.0
        # Level: amplitude of the best-fitting enveloped tone, in dBFS.
        amp = 2.0 * math.sqrt(max(best, 0.0)) / sum(e * e for e in env)
        level = max(-120.0, 20.0 * math.log10(max(amp, 1e-9) / 32768.0))
        # Duration: 10 ms windows from 100 ms before to 400 ms after the onset.
        win = rate // 100
        lo_d = max(0, onset - 10 * win)
        powers = [goertzel(left[i:i + win], rate, 880.0)
                  for i in range(lo_d, min(len(left) - win, onset + 40 * win), win)]
        top = max(powers) if powers else 0.0
        dur_ms = 10 * sum(1 for x in powers if top > 0 and x >= top / 10.0)
        good = abs(err_ms) <= a.tol_ms
        ok &= good
        print(f"beep expected at {t:.3f} s: found at {onset / rate:.3f} s ({err_ms:+.1f} ms) "
              f"{'ok' if good else 'FAIL'}, level {level:.1f} dBFS, duration {dur_ms} ms")
    print("PASS" if ok else "FAIL")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
