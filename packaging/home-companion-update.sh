#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Installs a release of home-companion when the body asks (Settings -> Info -> Install): run as
# root by home-companion-update.service, started by home-companion-update.path when the body
# writes a release tag to /var/lib/home-companion/update/request.
#
# Trust: the repository comes from the root-owned configuration only ([update] repo in
# /etc/home-companion/home-companion.toml), never from the body; the body only chooses a tag of
# it, with the bundle's SHA-256 the user saw and confirmed on the screen: only a bundle with exactly
# that hash, the same as in the release's SHA256SUMS, is installed. What runs now is kept, and put
# back if the new body does not stay up.
#
# It says how it goes in /var/lib/home-companion/update/status (key=value lines, read by the body).
set -u

STATE=/var/lib/home-companion/update
REQ=$STATE/request
STATUS=$STATE/status
CONF=/etc/home-companion/home-companion.toml
OPT=/opt/home-companion
UNITS=/etc/systemd/system
KEEP=/var/lib/home-companion-update/previous
WORK=/var/tmp/home-companion-update
DEFAULT_REPO=m0s4ik/home-companion
# The body must be up this long after the install, without systemd restarting it.
SETTLE=60

VERSION=
# What the screen shows (update.rs, Status): the step under way, its progress, why it stopped.
STEP=starting
BYTES=0
TOTAL=0
LEFT=0
status() { # phase message
    tmp=$STATUS.tmp
    printf 'phase=%s\nstep=%s\nversion=%s\nmessage=%s\nbytes=%s\ntotal=%s\nleft=%s\nat=%s\n' \
        "$1" "$STEP" "$VERSION" "$2" "$BYTES" "$TOTAL" "$LEFT" "$(date +%s)" > "$tmp" &&
        chown home-companion:home-companion "$tmp" 2>/dev/null
    mv -f "$tmp" "$STATUS"
}
step() { # phase: the step under way
    STEP=$1
    status "$1" ""
    echo "home-companion-update: $1 $VERSION"
}
fail() {
    echo "home-companion-update: failed during $STEP: $1"
    status failed "$1"
    rm -rf "$WORK"
    exit 1
}

# ---------------------------------------------------------------- the request
[ -f "$REQ" ] && [ ! -L "$REQ" ] || exit 0
# "<tag> <sha256>"
LINE=$(head -c 160 "$REQ" | head -n 1 | tr -d '\r')
rm -f "$REQ"
TAG=${LINE%% *}
SHA=${LINE#* }
case $TAG in
    ""|*[!A-Za-z0-9._-]*) fail "not a release tag" ;;
esac
case $SHA in
    *[!0-9a-f]*) fail "no confirmed SHA-256" ;;
esac
[ ${#SHA} -eq 64 ] || fail "no confirmed SHA-256"
VERSION=${TAG#v}

# The repository: [update] repo of the configuration, else ours.
REPO=$(awk '
    /^[[:space:]]*\[/ { sect = $0; gsub(/[[:space:]]/, "", sect) }
    sect == "[update]" && /^[[:space:]]*repo[[:space:]]*=/ {
        sub(/^[^=]*=[[:space:]]*"/, ""); sub(/".*$/, ""); print; exit
    }' "$CONF" 2>/dev/null)
[ -n "$REPO" ] || REPO=$DEFAULT_REPO
case $REPO in
    */*/*|/*|*/|*..*|*[!A-Za-z0-9._/-]*) fail "[update] repo is not owner/name: $REPO" ;;
    */*) ;;
    *) fail "[update] repo is not owner/name: $REPO" ;;
esac

case $(uname -m) in
    aarch64) ARCH=arm64 ;;
    x86_64) ARCH=amd64 ;;
    *) ARCH=$(uname -m) ;;
esac
NAME=home-companion-$VERSION-$ARCH
BASE=https://github.com/$REPO/releases/download/$TAG

# ---------------------------------------------------------------- download and check
step downloading
rm -rf "$WORK"
mkdir -m 700 "$WORK" || fail "cannot create $WORK"
# Its size first, then the download in the background, the bytes written to the status every second.
TOTAL=$(curl -fsSLI --max-time 30 "$BASE/$NAME.tar.gz" | tr -d '\r' |
    awk 'tolower($1) == "content-length:" { n = $2 } END { print n + 0 }')
curl -fsSL --max-time 900 -o "$WORK/$NAME.tar.gz" "$BASE/$NAME.tar.gz" &
DL=$!
while kill -0 "$DL" 2>/dev/null; do
    BYTES=$(stat -c %s "$WORK/$NAME.tar.gz" 2>/dev/null || echo 0)
    status downloading ""
    sleep 1
done
wait "$DL" || fail "download of $NAME.tar.gz"
BYTES=$(stat -c %s "$WORK/$NAME.tar.gz")
status downloading ""
curl -fsSL --max-time 60 -o "$WORK/SHA256SUMS" "$BASE/SHA256SUMS" || fail "download of SHA256SUMS"
step checking
GOT=$(sha256sum "$WORK/$NAME.tar.gz" | cut -d' ' -f1)
[ "$GOT" = "$SHA" ] || fail "$NAME.tar.gz has SHA-256 $GOT, not the one confirmed"
awk -v h="$SHA" -v n="$NAME.tar.gz" 'tolower($1) == h && ($2 == n || $2 == "*" n) { f = 1 } END { exit !f }' \
    "$WORK/SHA256SUMS" ||
    fail "$NAME.tar.gz is not in SHA256SUMS with the confirmed SHA-256"
tar -C "$WORK" -xzf "$WORK/$NAME.tar.gz" || fail "the bundle does not unpack"
[ -x "$WORK/$NAME/install.sh" ] || fail "no install.sh in the bundle"

# ---------------------------------------------------------------- keep what runs now
rm -rf "$KEEP"
# shellcheck disable=SC2015
mkdir -p "${KEEP%/*}" && chmod 700 "${KEEP%/*}" && mkdir -p "$KEEP/units" || fail "cannot create $KEEP"
cp -a "$OPT" "$KEEP/opt" || fail "cannot keep $OPT"
for f in "$UNITS"/home-companion*; do
    [ -e "$f" ] && cp -a "$f" "$KEEP/units/"
done
[ -f /etc/nftables.d/home-companion.nft ] && cp -a /etc/nftables.d/home-companion.nft "$KEEP/"

restore() {
    rm -rf "$OPT.new"
    cp -a "$KEEP/opt" "$OPT.new" && rm -rf "$OPT" && mv "$OPT.new" "$OPT"
    # units the new version added and the old one does not know: stopped and gone (a socket
    # unit would otherwise keep listening for a program that is no longer there)
    for f in "$UNITS"/home-companion*; do
        [ -e "$f" ] || continue
        n=${f##*/}
        if [ ! -e "$KEEP/units/$n" ]; then
            case "$n" in
                *.service | *.socket | *.path | *.timer) systemctl disable --now "$n" 2>/dev/null || true ;;
            esac
        fi
    done
    rm -rf "$UNITS"/home-companion*
    cp -a "$KEEP/units/." "$UNITS/"
    [ -f "$KEEP/home-companion.nft" ] && cp -a "$KEEP/home-companion.nft" /etc/nftables.d/
    systemctl daemon-reload
    systemctl restart home-companion-brain.service home-companion.service
}

# ---------------------------------------------------------------- install
step installing
if ! "$WORK/$NAME/install.sh" --yes > "$STATE/install.log" 2>&1; then
    restore
    echo "home-companion-update: install.sh failed, the version before is back"
    status rolled-back "install.sh failed (see $STATE/install.log)"
    rm -rf "$WORK"
    exit 1
fi
chown home-companion:home-companion "$STATE/install.log" 2>/dev/null

# ---------------------------------------------------------------- does it stay up?
step waiting
restarts=$(systemctl show -p NRestarts --value home-companion.service)
LEFT=$SETTLE
stable=yes
while [ "$LEFT" -gt 0 ]; do
    status waiting ""
    sleep 2
    LEFT=$((LEFT - 2))
    if ! systemctl -q is-active home-companion.service ||
        [ "$(systemctl show -p NRestarts --value home-companion.service)" != "$restarts" ]; then
        stable=no
        break
    fi
done
LEFT=0
if [ "$stable" = no ]; then
    restore
    echo "home-companion-update: the new version did not stay up, the version before is back"
    status rolled-back "the new version did not stay up"
    rm -rf "$WORK"
    exit 1
fi
STEP="done"
status "done" ""
echo "home-companion-update: done $VERSION"
rm -rf "$WORK"
