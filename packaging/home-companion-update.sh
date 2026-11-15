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
# it. The bundle is checked against the release's SHA256SUMS (over HTTPS from GitHub). What runs
# now is kept, and put back if the new body does not stay up.
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
status() { # phase message
    tmp=$STATUS.tmp
    printf 'phase=%s\nversion=%s\nmessage=%s\nat=%s\n' "$1" "$VERSION" "$2" "$(date +%s)" > "$tmp" &&
        chown home-companion:home-companion "$tmp" 2>/dev/null
    mv -f "$tmp" "$STATUS"
    echo "home-companion-update: $1 $VERSION $2"
}
fail() {
    status failed "$1"
    rm -rf "$WORK"
    exit 1
}

# ---------------------------------------------------------------- the request
[ -f "$REQ" ] && [ ! -L "$REQ" ] || exit 0
TAG=$(head -c 64 "$REQ" | tr -d '\n\r ')
rm -f "$REQ"
case $TAG in
    ""|*[!A-Za-z0-9._-]*) fail "not a release tag" ;;
esac
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
status downloading "$REPO"
rm -rf "$WORK"
mkdir -m 700 "$WORK" || fail "cannot create $WORK"
curl -fsSL --max-time 900 -o "$WORK/$NAME.tar.gz" "$BASE/$NAME.tar.gz" || fail "download of $NAME.tar.gz"
curl -fsSL --max-time 60 -o "$WORK/SHA256SUMS" "$BASE/SHA256SUMS" || fail "download of SHA256SUMS"
status checking ""
(cd "$WORK" && grep " $NAME.tar.gz\$" SHA256SUMS | sha256sum -c --status) ||
    fail "$NAME.tar.gz does not match SHA256SUMS"
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
    rm -rf "$UNITS"/home-companion*
    cp -a "$KEEP/units/." "$UNITS/"
    [ -f "$KEEP/home-companion.nft" ] && cp -a "$KEEP/home-companion.nft" /etc/nftables.d/
    systemctl daemon-reload
    systemctl restart home-companion-brain.service home-companion.service
}

# ---------------------------------------------------------------- install
status installing ""
if ! "$WORK/$NAME/install.sh" --yes > "$STATE/install.log" 2>&1; then
    restore
    status rolled-back "install.sh failed (see $STATE/install.log)"
    rm -rf "$WORK"
    exit 1
fi
chown home-companion:home-companion "$STATE/install.log" 2>/dev/null

# ---------------------------------------------------------------- does it stay up?
status waiting ""
restarts=$(systemctl show -p NRestarts --value home-companion.service)
sleep "$SETTLE"
if ! systemctl -q is-active home-companion.service ||
    [ "$(systemctl show -p NRestarts --value home-companion.service)" != "$restarts" ]; then
    restore
    status rolled-back "the new version did not stay up"
    rm -rf "$WORK"
    exit 1
fi
status "done" ""
rm -rf "$WORK"
