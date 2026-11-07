#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Device side of the UI spike runbook, run as root in the
# spike directory (default /tmp/home-companion-spike). Nothing outside that directory is
# written, apart from stopping and starting systemd units and the amplifier level.
#
#   device-session.sh free      find who holds the screen and the sound card,
#                               stop those units, mute the amplifier, then check
#   device-session.sh check     exit 1 (and say who) if any process still holds
#                               /dev/fb0, /dev/dri/card0 or /dev/snd/pcmC0D*
#   device-session.sh restore   start the stopped units again, restore the amplifier
#
# Unit names differ between images, so they are discovered, never assumed: the
# holders are found like `fuser -v` does (open files in /proc/PID/fd and
# mappings in /proc/PID/maps; psmisc is not needed), and the unit of each PID
# is read from /proc/PID/cgroup (the same answer as `systemctl status PID`).
# The units stopped are kept in units-stopped.txt for 'restore'.
set -eu

dir="${HOME_COMPANION_SPIKE_DIR:-/tmp/home-companion-spike}"
cd "$dir"
amp="Speaker Amp Master"

# HOME_COMPANION_SPIKE_DEVICES overrides the list (only to test this script elsewhere).
devices() {
    # shellcheck disable=SC2086 # the override is a space separated list
    for d in ${HOME_COMPANION_SPIKE_DEVICES:-/dev/fb0 /dev/dri/card0 /dev/snd/pcmC0D*}; do
        if [ -e "$d" ]; then printf '%s\n' "$d"; fi
    done
}

# PIDs with one of the devices open or mapped, one per line.
holders() {
    devs="$(devices)"
    for p in /proc/[0-9]*; do
        pid="${p#/proc/}"
        if { ls -l "$p/fd" 2>/dev/null; cat "$p/maps" 2>/dev/null; } |
            DEVS="$devs" awk 'BEGIN { n = split(ENVIRON["DEVS"], a, "\n"); for (i = 1; i <= n; i++) want[a[i]] = 1 }
                                 want[$NF] { found = 1 } END { exit !found }'; then
            echo "$pid"
        fi
    done
}

# The system service of a PID, or nothing (user sessions, user services,
# scopes: those are reported and left to the person running the runbook).
unit_of() {
    sed -n -E '/^0::\/system\.slice\//{s|^0::/system\.slice/||; s|(\.service)(/.*)?$|\1|; s|.*/||; /\.service$/p;}' \
        "/proc/$1/cgroup" 2>/dev/null | head -n 1
}

# One line per holder: pid, unit, command line.
describe() {
    for p in "$@"; do
        echo "pid $p unit '$(unit_of "$p")' cmd: $(tr '\0' ' ' <"/proc/$p/cmdline" 2>/dev/null)"
    done
}

check() {
    if [ -z "$(devices)" ]; then
        echo "check: FAIL, none of /dev/fb0, /dev/dri/card0, /dev/snd/pcmC0D* exists (wrong machine?)" >&2
        return 1
    fi
    pids="$(holders)"
    if [ -z "$pids" ]; then
        echo "check: ok, nobody holds the screen or the sound card"
        return 0
    fi
    echo "check: FAIL, still in use (stop these before measuring):" >&2
    # shellcheck disable=SC2086 # one argument per pid
    describe $pids >&2
    return 1
}

case "${1:-}" in
free)
    if [ -s units-stopped.txt ]; then
        echo "free: units-stopped.txt exists already; run 'restore' first" >&2
        exit 1
    fi
    pids="$(holders)"
    # shellcheck disable=SC2086
    describe $pids | tee owners-before.txt
    units=""
    for p in $pids; do
        u="$(unit_of "$p")"
        if [ -z "$u" ]; then
            echo "free: pid $p is not in a system service ($(cat "/proc/$p/cgroup" 2>/dev/null)): stop it by hand" >&2
            continue
        fi
        case " $units " in *" $u "*) ;; *) units="$units $u" ;; esac
    done
    # The amplifier level before anything changes, for 'restore'.
    amixer -c 0 sget "$amp" >amp-before.txt
    amixer -c 0 cget name="$amp Playback Volume" | sed -n 's/^ *: values=\([0-9]*\).*/\1/p' >amp-level.txt
    # shellcheck disable=SC2086 # one line per unit
    printf '%s\n' $units | sed '/^$/d' >units-stopped.txt
    echo "free: stopping:${units:- nothing} (list in $dir/units-stopped.txt)"
    if [ -n "$units" ]; then
        # shellcheck disable=SC2086 # one argument per unit
        systemctl stop $units
    fi
    amixer -c 0 sset "$amp" 0 >/dev/null
    echo "free: amplifier muted (was $(cat amp-level.txt))"
    check
    ;;
check)
    check
    ;;
restore)
    if [ -s units-stopped.txt ]; then
        # shellcheck disable=SC2046 # one argument per unit
        systemctl start $(cat units-stopped.txt)
        # shellcheck disable=SC2046
        systemctl is-active $(cat units-stopped.txt) || true
    fi
    if [ -s amp-level.txt ]; then
        amixer -c 0 sset "$amp" "$(cat amp-level.txt)" >/dev/null
    else
        echo "restore: amp-level.txt is empty: set '$amp' by hand (see amp-before.txt)" >&2
    fi
    amixer -c 0 sget "$amp"
    if [ -e units-stopped.txt ]; then mv units-stopped.txt units-restored.txt; fi
    ;;
*)
    echo "usage: $0 free|check|restore" >&2
    exit 2
    ;;
esac
