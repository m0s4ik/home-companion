#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Run vosk-grammar over the 12 Italian WAVs (fixtures/generated/vosk/, see make-wavs.sh) with the
# closed grammar fixtures/grammar-it.json and print a table. The expected text of each file is in
# frasi.txt ("name|sentence"); files named fuori* are out-of-grammar negatives.
# Fails (exit 1) unless at least 8 of the 9 commands give exactly the expected phrase and the
# overall real-time factor is below MAX_RTF (default 0.5).
#
# Environment:
#   VOSK_GRAMMAR  the binary (default: ../../target/release/vosk-grammar, else bin/vosk-grammar)
#   RUNNER        a prefix, e.g. "qemu-aarch64 -L /usr/aarch64-linux-gnu" or "nice -n 10 taskset -c 3"
#   VOSK_LIB_DIR  directory with libvosk.so (default third_party/vosk/$(uname -m), if present)
#   VOSK_MODEL    model directory (default models/vosk-it)
#   WAVS          directory with the WAVs and frasi.txt (default fixtures/generated/vosk)
#   MIN_CONF      reflex threshold (default 0.8)   MAX_RTF  (default 0.5)
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
bin=${VOSK_GRAMMAR:-}
if [ -z "$bin" ]; then
    if [ -x "$here/../../target/release/vosk-grammar" ]; then
        bin="$here/../../target/release/vosk-grammar"
    else
        bin="$here/bin/vosk-grammar"
    fi
fi
# libvosk.so: next to the binary (rpath $ORIGIN) on the device, from third_party here.
libdir=${VOSK_LIB_DIR:-$here/third_party/vosk/$(uname -m)}
if [ -f "$libdir/libvosk.so" ]; then
    export LD_LIBRARY_PATH="$libdir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
read -r -a runner <<<"${RUNNER:-}"
model=${VOSK_MODEL:-$here/models/vosk-it}
wavs=${WAVS:-$here/fixtures/generated/vosk}
grammar="$here/fixtures/grammar-it.json"
if [ ! -f "$wavs/frasi.txt" ]; then
    echo "error: $wavs/frasi.txt missing (run tools/make-wavs.sh)" >&2
    exit 2
fi

results=$(mktemp)
trap 'rm -f "$results"' EXIT
while IFS='|' read -r name said; do
    [ -n "$name" ] || continue
    rc=0
    # Capture the whole output and keep the JSON (first) line here: piping into `head -n 1`
    # would close the pipe before the summary line and make the binary die with EPIPE.
    out=$("${runner[@]}" "$bin" --model "$model" --grammar "$grammar" --wav "$wavs/$name.wav" \
        --min-conf "${MIN_CONF:-0.8}" </dev/null) || rc=$?
    json=${out%%$'\n'*}
    if [ "$rc" -ge 2 ] || [ -z "$json" ]; then
        echo "error: vosk-grammar failed on $name.wav" >&2
        exit 2
    fi
    printf '%s|%s|%s\n' "$name" "$said" "$json" >>"$results"
done <"$wavs/frasi.txt"

python3 -I - "$results" "${MAX_RTF:-0.5}" <<'EOF'
import json, sys
rows = []
for line in open(sys.argv[1], encoding="utf-8"):
    name, said, js = line.rstrip("\n").split("|", 2)
    rows.append((name, said, json.loads(js)))
max_rtf = float(sys.argv[2])
print(f"{'file':11s} {'said':34s} {'got':34s} {'ok':3s} {'accepted':8s} {'min conf':8s} {'rtf':5s} {'final ms':8s}")
right = commands = 0
audio = decode = cpu = 0.0
for name, said, r in rows:
    is_cmd = not name.startswith("fuori")
    exact = r["text"] == said
    commands += is_cmd
    right += is_cmd and exact
    audio += r["audio_s"]; decode += r["decode_s"]; cpu += r["cpu_s"]
    mark = ("yes" if exact else "NO") if is_cmd else "neg"
    print(f"{name:11s} {said:34s} {r['text']:34s} {mark:3s} {'yes' if r['accepted'] else 'no':8s} "
          f"{r['min_word_conf']:8.2f} {r['rtf']:5.2f} {r['final_ms']:8.0f}")
    words = " ".join(f"{w['word']}:{w['conf']:.2f}" for w in r["words"]) or "(no words)"
    print(f"{'':11s}   words: {words}")
load = [r["model_load_s"] for _, _, r in rows]
rss = max(r["peak_rss_mb"] for _, _, r in rows)
rtf = decode / audio
print(f"\ncommands right: {right}/{commands}; total audio {audio:.1f} s, decode {decode:.2f} s, RTF {rtf:.3f}, "
      f"cpu {cpu:.2f} s; model load {min(load):.2f}-{max(load):.2f} s; peak RSS {rss:.0f} MB")
fa = [n for n, _, r in rows if n.startswith("fuori") and r["accepted"]]
print(f"negatives accepted as reflexes: {len(fa)} {fa or ''}")
ok = right >= commands - 1 and rtf < max_rtf
print("PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
EOF
