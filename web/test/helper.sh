#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Starts the web helper for the body's "Casa" screen on the device (tests, until P3 packages it):
# home-companion-web in the browser test root (WPE 2.54, Mesa 26.3, FD_MESA_DEBUG=sysmem) as the
# unprivileged `kiosk` user, WebKit's sandbox on, as the transient unit `home-companion-web-test.service`
# (a cgroup of its own: the body freezes it while "Casa" is hidden). The page and the login come
# from the body's configuration: `home-companion web-url` ([home_assistant] url + [web] path) and
# `home-companion web-login` (the token of [home_assistant] token_file as hassTokens JSON, piped to
# `--token-stdin`: never on a command line, never in a log).
#
#   helper.sh start BODY CONFIG [SCALE]   e.g. helper.sh start /root/p2-test/home-companion /root/p2-test/home-companion.toml
#   helper.sh stop
#   helper.sh status
#
# The socket is $SOCKDIR/home-companion-web.sock (default /run/home-companion-web/home-companion-web.sock = `[web] socket`'s
# default), a directory of the host bound into the test root, group `home-companion` (setgid), so the body's
# unit (user home-companion, ProtectHome) reaches it; the unit's `cgroup.freeze` is given to `home-companion` for the
# freezer. The log: `journalctl -u home-companion-web-test`. Nothing here survives a reboot. Expects
# home-companion-web built in $ROOT/home/kiosk/src/home-companion-web-p2 (`make` inside the test root).

set -eu

ROOT=${ROOT:-/root/browser-test/chroot}
SRC=${SRC:-/home/kiosk/src/home-companion-web-p2}
SOCKDIR=${SOCKDIR:-/run/home-companion-web}
UNIT=home-companion-web-test

case ${1:-} in
stop)
    systemctl stop "$UNIT" 2>/dev/null || true
    rm -f "$SOCKDIR/home-companion-web.sock"
    exit 0
    ;;
status)
    systemctl status "$UNIT" --no-pager | head -12
    exit 0
    ;;
start) ;;
*)
    sed -n '5,/^$/p' "$0" >&2
    exit 2
    ;;
esac

BODY=${2:?home-companion binary}
CONFIG=${3:?home-companion configuration}
SCALE=${4:-1.25}
[ -x "$ROOT$SRC/home-companion-web" ] || { echo "missing: $ROOT$SRC/home-companion-web" >&2; exit 1; }
URL=$("$BODY" web-url --config "$CONFIG") || exit 1
"$BODY" web-login --config "$CONFIG" > /dev/null || exit 1

cat > "$ROOT/root/inside-helper.sh" <<'EOF'
#!/bin/sh
mkdir -p /tmp/xdg-kiosk && chown kiosk /tmp/xdg-kiosk && chmod 700 /tmp/xdg-kiosk
rm -f /run/home-companion-web/home-companion-web.sock
exec su kiosk -s /bin/sh -c "export XDG_RUNTIME_DIR=/tmp/xdg-kiosk HOME=/home/kiosk FD_MESA_DEBUG=sysmem \
GBM_BACKENDS_PATH=/usr/local/lib/mesa-freedreno/gbm; cd /home/kiosk; umask 007; exec $1/home-companion-web \
--token-stdin --socket /run/home-companion-web/home-companion-web.sock --url '$2' --scale $3 --theme dark --hidden \
--data-dir /home/kiosk/home-companion-web/data --cache-dir /home/kiosk/home-companion-web/cache"
EOF
mkdir -p "$ROOT/.oldroot"
# The socket's directory: the helper (kiosk, uid 1000 in the test root) creates the socket, the
# body (group home-companion) connects to it.
GROUP=$(getent group home-companion > /dev/null && echo home-companion || echo root)
install -d -m 2770 -o 1000 -g "$GROUP" "$SOCKDIR"
rm -f "$SOCKDIR/home-companion-web.sock"
systemctl stop "$UNIT" 2>/dev/null || true
systemctl reset-failed "$UNIT" 2>/dev/null || true
# Standard input: a FIFO (root only) the login line is written into once; the helper keeps running
# after its end. Standard output goes to the journal.
FIFO=$(mktemp -u /run/home-companion-web-login.XXXXXX)
mkfifo -m 600 "$FIFO"
trap 'rm -f "$FIFO"' EXIT
systemd-run --unit="$UNIT" --no-block --quiet --collect -p StandardInput=file:"$FIFO" \
    -p Nice=10 -p CPUWeight=50 -p IOWeight=50 \
    unshare --mount --propagation private /bin/bash -c "
    set -e
    mount --bind $ROOT $ROOT
    for d in dev dev/pts dev/shm proc sys run/udev; do mkdir -p $ROOT/\$d; mount --bind /\$d $ROOT/\$d; done
    mkdir -p $ROOT/run/home-companion-web; mount --bind $SOCKDIR $ROOT/run/home-companion-web
    cd $ROOT; pivot_root . .oldroot; umount -l /.oldroot
    exec /bin/sh /root/inside-helper.sh '$SRC' '$URL' '$SCALE'"
timeout 20 sh -c '"$1" web-login --config "$2" > "$3"' sh "$BODY" "$CONFIG" "$FIFO" ||
    { echo "the helper did not take the login: journalctl -u $UNIT" >&2; exit 1; }
for _ in $(seq 1 100); do [ -S "$SOCKDIR/home-companion-web.sock" ] && break; sleep 0.2; done
[ -S "$SOCKDIR/home-companion-web.sock" ] || { echo "the helper did not start: journalctl -u $UNIT" >&2; exit 1; }
# The freezer: the body (user home-companion) writes the helper's cgroup.freeze.
FREEZE=/sys/fs/cgroup/system.slice/$UNIT.service/cgroup.freeze
[ "$GROUP" = home-companion ] && [ -e "$FREEZE" ] && chown home-companion "$FREEZE"
echo "helper: $UNIT, socket $SOCKDIR/home-companion-web.sock"
