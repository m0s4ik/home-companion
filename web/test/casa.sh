#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Test on the device: Home Assistant's frontend on the panel through our own code, with touch.
# home-companion-web (the helper, WPE WebKit, in the test root as the unprivileged `kiosk` user, WebKit's
# sandbox on) renders off screen and sends its frames over the socket to webview-viewer (KMS,
# stands in for the body). home-companion.service is DRM master, so it is stopped for the test and ALWAYS
# started again at the end (time up, Ctrl+C, or anything failing).
#
#   casa.sh [minutes] [path]        e.g. casa.sh 10 lovelace/0   (default: 10 minutes, Overview)
#
# While it runs, from another shell (the viewer forwards to the helper):
#   echo hide > /root/webview-test/casa/viewer.ctl        show, hide, reload, back,
#   echo "load http://.../profile" > .../viewer.ctl       png /root/webview-test/casa/panel.png,
#   echo "png /tmp/frame.png" > /root/webview-test/casa/helper.ctl  (helper: the frame as WPE drew it)
# Logs: /root/webview-test/casa/helper.log, viewer.log.
#
# Expects (device layout of the browser tests): the test root in /root/browser-test/chroot with
# WPE 2.54, Mesa 26.3 and home-companion-web built in /home/kiosk/src/home-companion-web; the token as hassTokens JSON in
# /root/browser-test/tok.json (passed on standard input, never on a command line or in a log);
# webview-viewer next to this script.

set -u

HA_URL="http://HOME-ASSISTANT:8123"   # Home Assistant's base URL: set it on the device only

MINUTES=${1:-10}
PAGE=${2:-lovelace/0}
HERE=$(cd "$(dirname "$0")" && pwd)
C=/root/browser-test/chroot
TOKEN=/root/browser-test/tok.json
OUT=$HERE/casa
SOCK_INSIDE=/tmp/home-companion-web.sock
SOCK=$C$SOCK_INSIDE
# The viewer runs outside the test root with Mesa 26.3 from it (the device's own Mesa is older).
MESA=$C/opt/mesa263-nowl/lib

case $MINUTES in '' | *[!0-9]*) echo "usage: $0 [minutes] [path]" >&2; exit 2 ;; esac
for f in "$C/home/kiosk/src/home-companion-web/home-companion-web" "$TOKEN" "$HERE/webview-viewer" "$MESA"; do
    [ -e "$f" ] || { echo "missing: $f" >&2; exit 1; }
done
case $HA_URL in *HOME-ASSISTANT*) echo "set HA_URL at the top of $0" >&2; exit 1 ;; esac
KIOSK_UID=$(chroot "$C" id -u kiosk) || { echo "no kiosk user in the test root" >&2; exit 1; }

mkdir -p "$OUT"
rm -f "$OUT/helper.ctl" "$OUT/viewer.ctl" "$SOCK"
mkfifo -m 600 "$OUT/helper.ctl" "$OUT/viewer.ctl"
# Held open read-write by this script: the readers never see end of file while it runs.
exec 3<>"$OUT/helper.ctl" 4<>"$OUT/viewer.ctl"

HELPER=""
VIEWER=""
HC_STOPPED=0
cleanup() {
    trap - EXIT INT TERM HUP
    echo "casa: stopping"
    [ -n "$VIEWER" ] && kill "$VIEWER" 2>/dev/null
    [ -n "$HELPER" ] && kill "$HELPER" 2>/dev/null
    pkill -TERM -U "$KIOSK_UID" 2>/dev/null
    for _ in 1 2 3 4 5 6; do
        pgrep -U "$KIOSK_UID" >/dev/null || pgrep -x webview-viewer >/dev/null || break
        sleep 0.5
    done
    pkill -KILL -U "$KIOSK_UID" 2>/dev/null
    pkill -KILL -x webview-viewer 2>/dev/null
    exec 3>&- 4>&-
    rm -f "$OUT/helper.ctl" "$OUT/viewer.ctl" "$SOCK"
    if [ "$HC_STOPPED" = 1 ] || ! systemctl is-active -q home-companion; then
        systemctl start home-companion
        echo "casa: home-companion $(systemctl is-active home-companion)"
    fi
    grep -E "dashboard (ready|drawn)|first frame|load failed|terminated|menu" "$OUT/helper.log" 2>/dev/null |
        head -20 | sed 's/^/  helper: /'
    grep -E "import buffer|show →|touch →|state:|disconnected|stats:" "$OUT/viewer.log" 2>/dev/null |
        tail -20 | sed 's/^/  viewer: /'
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

# The helper, inside the test root (own mount namespace) as kiosk. Its standard input: the token
# line first, then the control FIFO.
cat > "$C/root/inside-casa.sh" <<'EOF'
#!/bin/sh
mkdir -p /tmp/xdg-kiosk && chown kiosk /tmp/xdg-kiosk && chmod 700 /tmp/xdg-kiosk
exec su kiosk -s /bin/sh -c "export XDG_RUNTIME_DIR=/tmp/xdg-kiosk HOME=/home/kiosk FD_MESA_DEBUG=sysmem \
GBM_BACKENDS_PATH=/usr/local/lib/mesa-freedreno/gbm; cd /home/kiosk; exec /home/kiosk/src/home-companion-web/home-companion-web \
--token-stdin --socket '$1' --url '$2' --theme dark \
--data-dir /home/kiosk/home-companion-web/data --cache-dir /home/kiosk/home-companion-web/cache"
EOF
mkdir -p "$C/.oldroot"
URL="$HA_URL/$PAGE"
echo "casa: $URL for $MINUTES min; logs in $OUT"
{
    python3 -c 'import json,sys; print(json.dumps(json.load(open(sys.argv[1])), separators=(",", ":")))' "$TOKEN"
    exec cat "$OUT/helper.ctl"
} 3>&- 4>&- | unshare --mount --propagation private /bin/bash -c "
    set -e
    mount --bind $C $C
    for d in dev dev/pts dev/shm proc sys run/udev; do mkdir -p $C/\$d; mount --bind /\$d $C/\$d; done
    cd $C; pivot_root . .oldroot; umount -l /.oldroot
    exec /bin/sh /root/inside-casa.sh '$SOCK_INSIDE' '$URL'" > "$OUT/helper.log" 2>&1 3>&- 4>&- &
HELPER=$!

for _ in $(seq 1 100); do [ -S "$SOCK" ] && break; sleep 0.2; done
[ -S "$SOCK" ] || { echo "casa: the helper did not start (see $OUT/helper.log)" >&2; exit 1; }

# The body gives up the screen. If it is just restarting (a deploy), wait for it first.
for _ in $(seq 1 30); do
    state=$(systemctl is-active home-companion)
    [ "$state" = active ] || [ "$state" = inactive ] || [ "$state" = failed ] && break
    sleep 1
done
HC_STOPPED=1
systemctl stop home-companion

LD_LIBRARY_PATH=$MESA GBM_BACKENDS_PATH=$MESA/gbm LIBGL_DRIVERS_PATH=$MESA/dri \
    "$HERE/webview-viewer" --socket "$SOCK" --stats 30 < "$OUT/viewer.ctl" > "$OUT/viewer.log" 2>&1 3>&- 4>&- &
VIEWER=$!

END=$(( $(date +%s) + MINUTES * 60 ))
while [ "$(date +%s)" -lt "$END" ]; do
    kill -0 "$VIEWER" 2>/dev/null || { echo "casa: the viewer stopped (see $OUT/viewer.log)"; exit 1; }
    kill -0 "$HELPER" 2>/dev/null || { echo "casa: the helper stopped (see $OUT/helper.log)"; exit 1; }
    sleep 1
done
echo "casa: time is up"
