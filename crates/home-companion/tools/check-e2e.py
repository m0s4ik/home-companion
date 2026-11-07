#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
"""Checks an end-to-end run of home-companion against the fake Home Assistant (tools/run-e2e.sh).
Pure standard library; run with `python3 -I`.

usage: check-e2e.py OUT_DIR

OUT_DIR holds e2e-home-companion.log (home-companion's JSON log), e2e-record.json (fake-ha what-time),
e2e-timers.json (fake-ha timers), e2e-offline.json (fake-ha timer-offline),
e2e-probe.json (fake-ha probe-while-connected) and e2e-out.wav
(everything home-companion played, 48 kHz stereo). Prints one line per check and PASS or FAIL.
"""
import json
import math
import os
import sys
import wave

failures = []


def check(ok, what):
    print(f"{'ok  ' if ok else 'FAIL'} {what}")
    if not ok:
        failures.append(what)
    return ok


def load_log(path):
    lines = []
    with open(path, encoding="utf-8") as f:
        for raw in f:
            raw = raw.strip()
            if raw.startswith("{"):
                try:
                    lines.append(json.loads(raw))
                except json.JSONDecodeError:
                    pass
    return lines


def events(log, name, **match):
    return [e for e in log if e.get("event") == name and all(e.get(k) == v for k, v in match.items())]


def first_index(log, name, start=0, **match):
    for i in range(start, len(log)):
        e = log[i]
        if e.get("event") == name and all(e.get(k) == v for k, v in match.items()):
            return i
    return None


def check_record(path, label):
    with open(path, encoding="utf-8") as f:
        rec = json.load(f)
    check(rec.get("failures") == [], f"{label}: fake-ha failures {rec.get('failures')}")
    return rec


def goertzel_db(samples, rate, freq):
    """Amplitude of `freq` in `samples`, dBFS."""
    k = 2.0 * math.cos(2.0 * math.pi * freq / rate)
    s1 = s2 = 0.0
    for x in samples:
        s1, s2 = x + k * s1 - s2, s1
    power = max(s1 * s1 + s2 * s2 - k * s1 * s2, 0.0)
    amp = 2.0 * math.sqrt(power) / len(samples)
    return 20.0 * math.log10(max(amp, 1e-9) / 32768.0)


def wav_checks(path):
    with wave.open(path, "rb") as w:
        rate, ch, width, n = w.getframerate(), w.getnchannels(), w.getsampwidth(), w.getnframes()
        raw = w.readframes(n)
    check(rate == 48000 and width == 2 and ch == 2, f"output wav {rate} Hz {ch} ch {8 * width} bit ({n / rate:.1f} s)")
    left = [int.from_bytes(raw[i:i + 2], "little", signed=True) for i in range(0, len(raw), 2 * ch)]
    win = rate // 100  # 10 ms windows
    levels = []  # (t, db660, db880, db1100)
    for i in range(0, len(left) - win, win):
        seg = left[i:i + win]
        if max(abs(x) for x in seg) < 300:  # silence: skip the filters
            levels.append((i / rate, -120.0, -120.0, -120.0))
            continue
        levels.append((i / rate, goertzel_db(seg, rate, 660.0), goertzel_db(seg, rate, 880.0),
                       goertzel_db(seg, rate, 1100.0)))
    thr = -20.0
    wake_at = None
    for j, (t, d660, _, _) in enumerate(levels):
        if d660 > thr:
            # The wake cue: 660 Hz for 120 ms, then 880 Hz for 160 ms.
            after = [l for l in levels[j + 8:j + 25] if l[2] > thr]
            if after:
                wake_at = t
                break
    answer_at = next((t for (t, _, _, d1100) in levels if d1100 > thr), None)
    check(wake_at is not None, f"wake cue (660 -> 880 Hz) > {thr} dBFS in the output at {wake_at}")
    check(answer_at is not None, f"answer tone (1100 Hz) > {thr} dBFS in the output at {answer_at}")
    if wake_at is not None and answer_at is not None:
        check(wake_at < answer_at, f"wake cue ({wake_at:.2f} s) before the answer ({answer_at:.2f} s)")
        peak = max(l[3] for l in levels)
        print(f"     answer tone level {peak:.1f} dBFS")


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    out = sys.argv[1]
    log = load_log(os.path.join(out, "e2e-home-companion.log"))
    check(len(log) > 0, f"home-companion log has {len(log)} JSON lines")

    # what-time
    rec = check_record(os.path.join(out, "e2e-record.json"), "what-time")
    info = rec.get("info") or {}
    sat = info.get("satellite") or {}
    models = [m.get("name") for p in info.get("wake", []) for m in p.get("models", [])]
    check(bool(sat), f"info has satellite {sat.get('name')!r}")
    check("okay_alfred" in models, f"info wake models {models}")
    received = rec.get("received", [])
    det = [r for r in received if r[1] == "detection"]
    rp = [r for r in received if r[1] == "run-pipeline"]
    check(len(det) == 1 and det[0][0] < 12.0, f"detection received at {det[0][0] if det else None} s")
    check(len(det) == 1 and det[0][2].get("name") == "okay_alfred", "detection name okay_alfred")
    check(len(rp) == 1 and rp[0][2].get("start_stage") == "asr" and rp[0][2].get("end_stage") == "tts",
          f"run-pipeline asr -> tts {rp[0][2] if rp else None}")
    check(rec.get("audio_seconds", 0) >= 1.5, f"{rec.get('audio_seconds')} s of 16 kHz audio received")
    a2p = rec.get("audio_stop_to_played_ms")
    check(a2p is not None and a2p <= 1500 + 2000, f"played {a2p} ms after audio-stop (answer 1.5 s + 2 s)")

    convs = events(log, "conversation")
    wake = [c for c in convs if c.get("trigger") == "wake"]
    check(len(wake) == 1 and wake[0].get("outcome") == "played", f"conversation line {wake[0] if wake else None}")
    if wake:
        c = wake[0]
        d2r = c.get("detect_to_run_pipeline_ms", -1)
        s2p = c.get("audio_stop_to_played_ms", -1)
        check(0 <= d2r < 50, f"detection -> run-pipeline {d2r} ms (< 50)")
        check(0 <= s2p < 300, f"audio-stop -> played {s2p} ms (< 300, file backend)")
        check(c.get("heard") == "what time is it", f"heard {c.get('heard')!r}")
        check(c.get("answer") == "It is ten past two.", f"answer {c.get('answer')!r}")
        for k in ("run_pipeline_to_first_chunk_ms", "first_chunk_to_transcript_ms", "transcript_to_audio_start_ms",
                  "audio_start_to_first_sample_ms", "audio_seconds_sent", "answer_seconds"):
            print(f"     {k} = {c.get(k)}")
    ann = [c for c in convs if c.get("trigger") == "announcement"]
    check(len(ann) == 1 and ann[0].get("outcome") == "played", "announcement answered with played")

    started = first_index(log, "timer_started", id="t1")
    cancelled = first_index(log, "timer_cancelled", id="t1")
    check(started is not None and cancelled is not None and started < cancelled, "timer t1 mirrored, then cancelled")
    check(not events(log, "timer_rung", id="t1"), "cancelled timer t1 never rang")
    pause = first_index(log, "pause_satellite")
    off = first_index(log, "phase", pause or 0, phase="offline") if pause is not None else None
    check(pause is not None and off is not None, "pause-satellite -> phase offline")
    phases = [e.get("phase") for e in events(log, "phase")]
    for p in ("listening", "thinking", "speaking", "home"):
        check(p in phases, f"phase {p} reached")

    # timers: rung once by home-companion, Home Assistant's late timer-finished does not ring again
    check_record(os.path.join(out, "e2e-timers.json"), "timers")
    rung = events(log, "timer_rung", id="t2")
    check(len(rung) == 1, f"timer t2 rung {len(rung)} time(s) (exactly 1)")
    fin = events(log, "timer_finished", id="t2")
    check(len(fin) == 1 and fin[0].get("rung_locally") is True, "late timer-finished t2 matched, not rung again")

    # timer-offline: rings with Home Assistant gone
    check_record(os.path.join(out, "e2e-offline.json"), "timer-offline")
    rung = events(log, "timer_rung", id="t-offline")
    check(len(rung) == 1 and rung[0].get("connected") is False, f"timer t-offline rung while disconnected {rung}")
    removed = events(log, "timer_removed", id="t-offline")
    check(len(removed) == 1, "timer t-offline removed after its bell")

    # probe-while-connected: describe-only connections never replace the satellite link
    check_record(os.path.join(out, "e2e-probe.json"), "probe-while-connected")
    replaced = events(log, "disconnected", reason="replaced")
    check(not replaced, f"no satellite link replaced by a probe {replaced}")

    check(not [e for e in log if e.get("level") == "error"], "no error lines in the home-companion log")
    exit_lines = events(log, "exit")
    check(len(exit_lines) == 1 and exit_lines[0].get("code") == 0, "home-companion exited cleanly (exit 0)")

    wav_checks(os.path.join(out, "e2e-out.wav"))

    print("PASS" if not failures else f"FAIL ({len(failures)})")
    sys.exit(0 if not failures else 1)


if __name__ == "__main__":
    main()
