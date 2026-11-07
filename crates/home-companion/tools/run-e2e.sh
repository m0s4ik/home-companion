#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# End-to-end test: home-companion (file audio, headless screen) against the fake Home Assistant.
#   1. what-time: "okay alfred" from the capture WAV -> detection -> run-pipeline -> audio ->
#      transcript -> synthesize + answer audio played -> played; a timer mirrored and
#      cancelled; an announcement; pause-satellite -> offline.
#   2. timers: a 5 s timer rung by home-companion itself; Home Assistant's late timer-finished is not
#      rung again.
#   3. timer-offline: a 3 s timer, then Home Assistant disconnects: home-companion rings it alone.
#   4. probe-while-connected: describe-only connections while the satellite link is up (as Home
#      Assistant's load_wyoming_info does every ~30 s): the link must stay up.
# Then SIGTERM (clean shutdown) and tools/check-e2e.py on the log, the records and the output
# WAV. Needs a release build (cargo build --release -p home-companion -p wyoming). Plays no sound.
#
# usage: crates/home-companion/tools/run-e2e.sh [TARGET_DIR]   (default target/release)
set -eu
root=$(cd "$(dirname "$0")/../../.." && pwd)
bin=${1:-$root/target/release}
out=$root/crates/home-companion/out
cd "$root"
rm -rf "$out/e2e-state" "$out"/e2e-*
mkdir -p "$out"

timeout 150 "$bin/home-companion" --config crates/home-companion/ci/e2e.toml --renderer headless --exit-after 90 >"$out/e2e-home-companion.stderr" 2>&1 &
body=$!
cleanup() { kill "$body" 2>/dev/null || true; }
trap cleanup EXIT

# Wait for the Wyoming port (the "listening" log line), at most 20 s.
i=0
until grep -q '"event":"listening"' "$out/e2e-home-companion.log" 2>/dev/null; do
    i=$((i + 1))
    if [ $i -gt 200 ] || ! kill -0 "$body" 2>/dev/null; then
        echo "run-e2e: home-companion did not start"; cat "$out/e2e-home-companion.stderr"; tail -20 "$out/e2e-home-companion.log" 2>/dev/null || true
        exit 1
    fi
    sleep 0.1
done

rc=0
echo "run-e2e: fake-ha what-time"
timeout 120 "$bin/fake-ha" --connect 127.0.0.1:10700 --builtin what-time --out "$out/e2e-record.json" || rc=1
echo "run-e2e: fake-ha timers"
timeout 120 "$bin/fake-ha" --connect 127.0.0.1:10700 --builtin timers --out "$out/e2e-timers.json" || rc=1
echo "run-e2e: fake-ha timer-offline"
timeout 120 "$bin/fake-ha" --connect 127.0.0.1:10700 --scenario crates/home-companion/ci/timer-offline.toml --out "$out/e2e-offline.json" || rc=1
# The offline timer rings 3 s after it started; its bell (2 rings) lasts about 3 s.
sleep 7
echo "run-e2e: fake-ha probe-while-connected"
timeout 120 "$bin/fake-ha" --connect 127.0.0.1:10700 --builtin probe-while-connected --out "$out/e2e-probe.json" || rc=1

kill -TERM "$body"
home_companion_rc=0
wait "$body" || home_companion_rc=$?
trap - EXIT
echo "run-e2e: home-companion exited with $home_companion_rc"
[ "$home_companion_rc" -eq 0 ] || rc=1

python3 -I crates/home-companion/tools/check-e2e.py "$out" || rc=1
exit $rc
