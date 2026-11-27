#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# The audio did not start (exit 6, crates/home-companion/src/audio_down.rs): reboot once for it.
# Run as root by home-companion.service (ExecStopPost=+) after every stop of the body, with
# $EXIT_STATUS set by systemd. Usage: home-companion-audio-reboot.sh [mark]
#
# Reboots only when all hold:
# - no update runs: the updater (home-companion-update.sh) is watching the new version and puts
#   the one before back if it does not stay up;
# - the body exited with status 6;
# - the mark is not there (the body removes it after 10 minutes of working audio), and is not
#   a link (the directory belongs to the body's user: never followed);
# - at least 5 minutes since booting (the sound card may still be coming up).
# The mark is made as a new file and renamed into place before the reboot, and goes again if
# the reboot is refused. Always exits 0: Restart= then works as for any failure.
#
# The mark's path must be <[paths] state_dir>/audio-reboot, where the body looks for it
# (packaging/home-companion.toml.in). HC_UPTIME_FILE is for the tests only.
set -u

m=${1:-/var/lib/home-companion/audio-reboot}
uptime_file=${HC_UPTIME_FILE:-/proc/uptime}

case "$(systemctl show -p ActiveState --value home-companion-update.service)" in
    activating | active | deactivating | reloading) exit 0 ;;
esac
if [ "${EXIT_STATUS:-}" = 6 ] && [ ! -L "$m" ] && [ ! -e "$m" ] &&
    [ "$(cut -d. -f1 "$uptime_file")" -ge 300 ]; then
    echo "audio did not start: rebooting once for it"
    t=$(mktemp "$m.XXXXXX") && mv -fT "$t" "$m" && { systemctl --no-block reboot || rm -f "$m"; }
fi
exit 0
