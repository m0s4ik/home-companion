#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Installs, upgrades or removes home-companion on a ThinkSmart View running thinksmart-view-debian
# (Debian 13 arm64 with Mesa freedreno as the package mesa-freedreno). Run as root from the unpacked
# release bundle:
#
#   ./install.sh                     # the language and Home Assistant: on the device's screen
#   ./install.sh --language it --ha-url http://192.0.2.10:8123
#   ./install.sh --dry-run           # prints what it would do, changes nothing
#   ./install.sh --uninstall [--purge]
#
# Idempotent: run it again (or a newer bundle's) to upgrade; the configuration in /etc/home-companion and
# the state in /var/lib/home-companion* are kept, the services restarted. Layout: packaging/README.md.
# The device must already be on the network (Wi-Fi is set when the system image is built).
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
PREFIX=/opt/home-companion
ETC=/etc/home-companion
UNITS=/etc/systemd/system
WEB_MESA_DROPIN=$UNITS/home-companion-web.service.d/mesa-gmem.conf
MESA_GMEM_FIXED=26.3.0-2
UDEV_RULE=/etc/udev/rules.d/99-home-companion-backlight.rules
NFT_FILE=/etc/nftables.d/home-companion.nft
ARCH_LIBS=/usr/lib/aarch64-linux-gnu

# The Vosk model of each language the reflexes know (a characters/*/grammar/<lang>.toml in the bundle):
# language, model name (https://alphacephei.com/vosk/models, Apache-2.0), sha256 of its zip.
VOSK_URL=https://alphacephei.com/vosk/models
VOSK_MODELS="
it vosk-model-small-it-0.22 9ec65e75861d1c6c2e457cccd932705340dcdf233f5b239f00733b4de0bf3267
en vosk-model-small-en-us-0.15 30f26242c4eb449f948e42cb302dd7a686cb29a3423a8367f99ff41780942498
nl vosk-model-small-nl-0.22 039811c3b829de64e4f123a9f684a53784005b212a346ac0b899dc7efce2ed0a
"

# Debian packages: what the body and its helpers load (Mesa's libgbm/libEGL come from mesa-freedreno),
# what the installer itself uses (python3, curl), avahi-browse (the first-start screen finds Home Assistant
# with it), and the AirPlay runtime (thinksmart-view-debian's AIRPLAY=1 layer).
CORE_PACKAGES="libasound2t64 libdrm2 libstdc++6 libgcc-s1 libatomic1 python3 curl ca-certificates nftables avahi-daemon avahi-utils"
AIRPLAY_PACKAGES="gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-libav libgstreamer-plugins-bad1.0-0 libplist-2.0-4 libavahi-compat-libdnssd1 libssl3t64"
# Of gstreamer1.0-plugins-bad UxPlay needs only these two (the image's AIRPLAY=1 layer has them in a small
# gstreamer1.0-plugins-bad-airplay package; without it the whole gstreamer1.0-plugins-bad, ~210 MB)
AIRPLAY_BAD_PLUGINS="libgstvideoparsersbad.so libgstunixfd.so"
AIRPLAY_PLUGINS="libgstapp.so libgstaudioconvert.so libgstaudioresample.so libgstvideo4linux2.so libgstdebug.so libgstlibav.so $AIRPLAY_BAD_PLUGINS"
WPE_MIN=2.54

usage() {
    cat <<'EOF'
usage: install.sh [options]

Nothing is asked (but whether to keep an existing configuration): the language and Home Assistant's
address are chosen on the device's screen at first start unless given here.

  --language it|en|nl   the language
  --name NAME           the device's name in Home Assistant and in the AirPlay menu (default Alfred)
  --ha-url URL          Home Assistant's address, http://host:8123
  --no-airplay          no AirPlay receiver (default: on when the system has the AirPlay runtime,
                        thinksmart-view-debian AIRPLAY=1)
  --airplay             AirPlay on, installing its runtime's Debian packages when they are missing
  --no-web              no Home Assistant view on the screen (default: on when the system has
                        WPE WebKit 2.54, thinksmart-view-debian WEB=1)
  --web                 the view on, or stop when WPE WebKit 2.54 is missing
  --no-reflexes         no reflexes: the Vosk models are not downloaded
  --keep-config         keep an existing configuration without asking (the default with --yes)
  --new-config          write a new configuration over an existing one (the old one is kept as *.prev)
  --yes, -y             do not ask whether to keep an existing configuration: keep it (also when
                        standard input is not a terminal)
  --dry-run             print what would be done, change nothing
  --uninstall           stop and remove home-companion (configuration and state are kept)
  --purge               with --uninstall: also the configuration, the state and the users
  -h, --help            this text
EOF
}

LANGUAGE='' DEVICE_NAME='' HA_URL='' AIRPLAY='' WEB='' REFLEXES=yes CONFIG_MODE='' YES='' DRY='' UNINSTALL='' PURGE=''
_keep=''
while [ $# -gt 0 ]; do
    case "$1" in
    --language) LANGUAGE=${2:?--language needs a value}; shift ;;
    --language=*) LANGUAGE=${1#*=} ;;
    --name) DEVICE_NAME=${2:?--name needs a value}; shift ;;
    --name=*) DEVICE_NAME=${1#*=} ;;
    --ha-url) HA_URL=${2:?--ha-url needs a value}; shift ;;
    --ha-url=*) HA_URL=${1#*=} ;;
    --airplay) AIRPLAY=yes ;;
    --no-airplay) AIRPLAY=no ;;
    --web) WEB=yes ;;
    --no-web) WEB=no ;;
    --no-reflexes) REFLEXES=no ;;
    --keep-config) CONFIG_MODE=keep ;;
    --new-config) CONFIG_MODE=new ;;
    --yes | -y) YES=1 ;;
    --dry-run) DRY=1 ;;
    --uninstall) UNINSTALL=1 ;;
    --purge) PURGE=1 ;;
    -h | --help) usage; exit 0 ;;
    *) echo "install.sh: unknown option $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done
[ -t 0 ] || YES=1

say() { printf '==> %s\n' "$*"; }
note() { printf '    %s\n' "$*"; }
warn() { printf 'WARNING: %s\n' "$*" >&2; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
# A change to the system: printed in a dry run, done otherwise.
run() {
    if [ -n "$DRY" ]; then
        printf '    + %s\n' "$*"
    else
        "$@"
    fi
}
installed() { dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q 'install ok installed'; }
pkg_version() { dpkg-query -W -f='${Version}' "$1" 2>/dev/null; }
have_unit() { [ -f "$UNITS/$1" ]; }

# yesno VAR "question" yes|no: on a terminal the answer (Enter = the default); with --yes the default.
yesno() {
    if [ -n "$YES" ]; then
        eval "$1=\$3"
        return
    fi
    while :; do
        printf '%s (yes/no) [%s]: ' "$2" "$3"
        read -r _yn || _yn=
        [ -n "$_yn" ] || _yn=$3
        case "$_yn" in
        y | yes) eval "$1=yes"; return ;;
        n | no) eval "$1=no"; return ;;
        esac
        echo "    yes or no"
    done
}

{ [ -f "$HERE/VERSION" ] && [ -d "$HERE/opt" ]; } || die "run install.sh from the unpacked release bundle ($HERE has no VERSION/opt)"
BUNDLE_VERSION=$(sed -n 's/^version=//p' "$HERE/VERSION")
if [ "$(id -u)" != 0 ]; then
    [ -n "$DRY" ] || die "run it as root"
    warn "not root: a dry run only, some checks may be incomplete"
fi
[ -n "$DRY" ] && say "dry run: nothing is changed"

# ---------------------------------------------------------------- uninstall
if [ -n "$UNINSTALL" ]; then
    say "removing home-companion $(sed -n 's/^version=//p' "$PREFIX/VERSION" 2>/dev/null || echo '(unknown version)')"
    for u in home-companion-web home-companion-brain home-companion; do
        have_unit "$u.service" && run systemctl disable --now "$u.service"
    done
    for f in "$UNITS/home-companion.service" "$UNITS/home-companion-web.service" "$UNITS/home-companion-brain.service" "$UDEV_RULE" "$WEB_MESA_DROPIN"; do
        [ -e "$f" ] && run rm -f "$f"
    done
    [ -d "$UNITS/home-companion.service.wants" ] && run rm -rf "$UNITS/home-companion.service.wants"
    if [ -e "$NFT_FILE" ]; then
        run rm -f "$NFT_FILE"
        systemctl -q is-active nftables 2>/dev/null && run systemctl reload nftables
    fi
    [ -d "$PREFIX" ] && run rm -rf "$PREFIX"
    run systemctl daemon-reload
    run udevadm control --reload
    if [ -n "$PURGE" ]; then
        for d in "$ETC" /var/lib/home-companion /var/lib/home-companion-brain /var/lib/home-companion-web /var/cache/home-companion-web; do
            [ -e "$d" ] && run rm -rf "$d"
        done
        for u in home-companion home-companion-web home-companion-brain; do
            getent passwd "$u" >/dev/null && run userdel "$u"
            getent group "$u" >/dev/null && run groupdel "$u"
        done
        say "removed, configuration, state and users included"
    else
        say "removed; kept: $ETC, /var/lib/home-companion*, /var/cache/home-companion-web, the users (--purge removes them)"
    fi
    note "Debian packages installed for home-companion stay (apt-get autoremove does not know them)."
    exit 0
fi

# ---------------------------------------------------------------- the system
say "home-companion $BUNDLE_VERSION"
[ "$(dpkg --print-architecture 2>/dev/null)" = arm64 ] || die "this bundle is for arm64 Debian; this system is $(dpkg --print-architecture 2>/dev/null || uname -m)"
# in a subshell: os-release's variables (NAME, VERSION...) stay out of this script
# shellcheck disable=SC1091
OS=$(. /etc/os-release && echo "${ID:-}-${VERSION_ID:-} ${PRETTY_NAME:-unknown}")
case "$OS" in debian-13\ *) ;; *) die "needs Debian 13 (trixie); this is ${OS#* }" ;; esac
[ -d /run/systemd/system ] || die "needs systemd"
installed mesa-freedreno || die "mesa-freedreno is not installed: home-companion needs thinksmart-view-debian's Mesa (v0.3-rc3 or later)"
note "Debian 13 arm64, mesa-freedreno $(pkg_version mesa-freedreno)"
[ -e "$ARCH_LIBS/libgbm.so.1" ] || ldconfig -p 2>/dev/null | grep -q 'libgbm\.so\.1 ' || die "libgbm.so.1 is missing (mesa-freedreno should provide it)"

PREVIOUS=$(sed -n 's/^version=//p' "$PREFIX/VERSION" 2>/dev/null || true)
if [ -n "$PREVIOUS" ]; then
    note "installed: $PREVIOUS (upgrade)"
elif [ -x "$PREFIX/home-companion" ]; then
    note "installed: an earlier copy without a version (upgrade)"
fi

# What the system offers
WEB_AVAILABLE=no WEB_WHY=
if [ ! -x "$HERE/opt/home-companion-web" ]; then
    WEB_WHY="the bundle has no web helper"
elif ! installed libwpewebkit-2.0-1; then
    WEB_WHY="WPE WebKit is not installed (thinksmart-view-debian built with WEB=1)"
elif ! dpkg --compare-versions "$(pkg_version libwpewebkit-2.0-1)" ge "$WPE_MIN"; then
    WEB_WHY="WPE WebKit $(pkg_version libwpewebkit-2.0-1) is older than $WPE_MIN (Debian's own does not work with Mesa 26)"
else
    WEB_AVAILABLE=yes
fi
AIRPLAY_RUNTIME=yes AIRPLAY_MISSING=
for p in $AIRPLAY_PLUGINS; do
    [ -e "$ARCH_LIBS/gstreamer-1.0/$p" ] || { AIRPLAY_RUNTIME=no; AIRPLAY_MISSING="$AIRPLAY_MISSING $p"; }
done
for l in libplist-2.0.so.4 libdns_sd.so.1; do
    [ -e "$ARCH_LIBS/$l" ] || { AIRPLAY_RUNTIME=no; AIRPLAY_MISSING="$AIRPLAY_MISSING $l"; }
done

# ---------------------------------------------------------------- the configuration
CONFIG_EXISTS=no
[ -f "$ETC/home-companion.toml" ] && CONFIG_EXISTS=yes
if [ "$CONFIG_EXISTS" = yes ]; then
    if [ -z "$CONFIG_MODE" ]; then
        yesno _keep "A configuration exists ($ETC/home-companion.toml). Keep it?" yes
        if [ "$_keep" = yes ]; then CONFIG_MODE=keep; else CONFIG_MODE=new; fi
    fi
else
    CONFIG_MODE=new
fi

if [ "$CONFIG_MODE" = new ]; then
    # Nothing is asked: the language and Home Assistant's address are chosen on the device's screen at
    # first start unless given here; the name is the character's
    case "$LANGUAGE" in "" | it | en | nl) ;; *) die "--language: it, en or nl, not $LANGUAGE" ;; esac
    [ -n "$DEVICE_NAME" ] || DEVICE_NAME=Alfred
    case "$DEVICE_NAME" in *'"'* | *\\* | *'|'* | *'&'*) die "--name: no quotes, backslashes, | or &" ;; esac
    HA_URL=${HA_URL%/}
    case "$HA_URL" in
    "" | http://?* | https://?*) ;;
    *) die "--ha-url: an http:// or https:// address, not $HA_URL" ;;
    esac
    case "$HA_URL" in *'"'* | *' '* | *'|'* | *'&'*) die "--ha-url: no quotes, spaces, | or &" ;; esac
    note "name \"$DEVICE_NAME\", language ${LANGUAGE:-chosen on the screen}, Home Assistant ${HA_URL:-found from the screen}"

    if [ "$AIRPLAY" = yes ]; then
        :
    elif [ "$AIRPLAY" = no ]; then
        :
    elif [ "$AIRPLAY_RUNTIME" = yes ]; then
        AIRPLAY=yes
        note "AirPlay: on (the system has the AirPlay runtime; --no-airplay turns it off)"
    else
        AIRPLAY=no
        note "AirPlay: off, the system has no AirPlay runtime (thinksmart-view-debian AIRPLAY=1; or --airplay installs it)"
    fi
    if [ "$WEB" = yes ]; then
        [ "$WEB_AVAILABLE" = yes ] || die "--web: $WEB_WHY"
    elif [ "$WEB" = no ]; then
        :
    elif [ "$WEB_AVAILABLE" = yes ]; then
        WEB=yes
        note "Home Assistant view: on (WPE WebKit $(pkg_version libwpewebkit-2.0-1); --no-web turns it off)"
    else
        WEB=no
        note "Home Assistant view: off, $WEB_WHY"
    fi
    if [ "$WEB" = yes ] && [ -z "$HA_URL" ]; then
        # The view waits for Home Assistant's address: the first-start screen finds it.
        note "Home Assistant view: on, it opens once the screen has found Home Assistant"
    fi
else
    # Kept: the features as they are in it
    note "configuration kept"
    grep -A3 '^\[airplay\]' "$ETC/home-companion.toml" 2>/dev/null | grep -q '^enabled *= *true' && AIRPLAY=${AIRPLAY:-yes}
    AIRPLAY=${AIRPLAY:-no}
fi
[ "$AIRPLAY" = yes ] && [ "$AIRPLAY_RUNTIME" = no ] && note "AirPlay runtime missing:$AIRPLAY_MISSING"

# ---------------------------------------------------------------- Debian packages
MISSING=
for p in $CORE_PACKAGES; do installed "$p" || MISSING="$MISSING $p"; done
if [ "$AIRPLAY" = yes ]; then
    for p in $AIRPLAY_PACKAGES; do installed "$p" || MISSING="$MISSING $p"; done
    for p in $AIRPLAY_BAD_PLUGINS; do
        if [ ! -e "$ARCH_LIBS/gstreamer-1.0/$p" ]; then
            MISSING="$MISSING gstreamer1.0-plugins-bad"
            warn "AirPlay: $p missing, gstreamer1.0-plugins-bad (~210 MB) will be installed; thinksmart-view-debian's gstreamer1.0-plugins-bad-airplay .deb is the small way"
            break
        fi
    done
fi
if [ -n "$MISSING" ]; then
    say "Debian packages to install:$MISSING"
    if ! ls /var/lib/apt/lists/*_Packages >/dev/null 2>&1; then
        run apt-get update -q
    fi
    # shellcheck disable=SC2086
    if SIM=$(apt-get -s -q install --no-install-recommends $MISSING 2>&1); then
        if printf '%s\n' "$SIM" | grep -q '^Remv '; then
            printf '%s\n' "$SIM" | grep '^Remv ' >&2
            die "apt would remove the packages above (mesa-freedreno must stay): install$MISSING by hand, then run install.sh again"
        fi
        note "$(printf '%s\n' "$SIM" | grep -c '^Inst ') packages, nothing removed"
        # shellcheck disable=SC2086
        run env DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends $MISSING
    elif [ -n "$DRY" ]; then
        warn "apt cannot install$MISSING yet (no package lists?): a real run updates them first"
    else
        printf '%s\n' "$SIM" | tail -n 5 >&2
        die "apt cannot install$MISSING"
    fi
else
    note "Debian packages: all there"
fi

# ---------------------------------------------------------------- users
ensure_user() { # name home
    if ! getent passwd "$1" >/dev/null; then
        run useradd --system --home-dir "$2" --no-create-home --shell /usr/sbin/nologin --user-group "$1"
    fi
}
say "users"
ensure_user home-companion /var/lib/home-companion
ensure_user home-companion-brain /var/lib/home-companion-brain
ensure_user home-companion-web /var/lib/home-companion-web
# the body reaches the brain's and the web helper's sockets through their groups
for g in home-companion-brain home-companion-web; do
    if ! id -nG home-companion 2>/dev/null | tr ' ' '\n' | grep -qx "$g"; then
        run usermod -aG "$g" home-companion
    fi
done

# ---------------------------------------------------------------- files
# put SRC DEST MODE: the new file next to the old, then renamed over it (a running program keeps its copy)
put() {
    run install -m "$3" "$1" "$2.new"
    run mv -f "$2.new" "$2"
}
# tree SRC DEST: DEST becomes a copy of SRC
tree() {
    run rm -rf "$2.new" "$2.old"
    run cp -R "$1" "$2.new"
    run chown -R root:root "$2.new"
    run chmod -R u+rwX,go+rX,go-w "$2.new"
    [ -e "$2" ] && run mv "$2" "$2.old"
    run mv "$2.new" "$2"
    run rm -rf "$2.old"
}
say "files in $PREFIX"
run install -d -m 0755 "$PREFIX" "$PREFIX/lib" "$PREFIX/airplay" "$PREFIX/reflex"
for b in home-companion home-companion-brain home-companion-reflex; do
    put "$HERE/opt/$b" "$PREFIX/$b" 0755
done
put "$HERE/opt/lib/libvosk.so" "$PREFIX/lib/libvosk.so" 0755
put "$HERE/opt/airplay/uxplay" "$PREFIX/airplay/uxplay" 0755
if [ "$WEB_AVAILABLE" = yes ]; then
    put "$HERE/opt/home-companion-web" "$PREFIX/home-companion-web" 0755
fi
tree "$HERE/opt/characters" "$PREFIX/characters"
tree "$HERE/opt/locales" "$PREFIX/locales"

# ---------------------------------------------------------------- Vosk models
# ensure_model LANG NAME SHA256: /opt/home-companion/reflex/LANG, downloaded, checked and slimmed once
ensure_model() {
    dest=$PREFIX/reflex/$1
    if [ -f "$dest/graph/words.txt" ] && [ -f "$dest/am/final.mdl" ]; then
        src=$(cat "$dest/.model" 2>/dev/null || true)
        if [ -z "$src" ] || [ "$src" = "$2" ]; then
            note "$1: $2 already there"
            return
        fi
    fi
    note "$1: $2 (download, check, slim)"
    [ -n "$DRY" ] && { printf '    + curl %s/%s.zip, unzip, slim-vosk-model.py, %s\n' "$VOSK_URL" "$2" "$dest"; return; }
    work=$PREFIX/reflex/.download-$1
    rm -rf "$work"
    mkdir -p "$work"
    curl -fL --retry 3 -sS -o "$work/$2.zip" "$VOSK_URL/$2.zip" || die "download of $2 failed"
    got=$(sha256sum "$work/$2.zip" | cut -d' ' -f1)
    [ "$got" = "$3" ] || die "$2.zip: sha256 $got, expected $3"
    python3 -I -m zipfile -e "$work/$2.zip" "$work/" || die "$2.zip: cannot unpack"
    python3 -I "$HERE/tools/slim-vosk-model.py" "$work/$2" >/dev/null || die "$2: slimming failed"
    rm -f "$work/$2.Gr.fst" "$work/$2.zip"
    echo "$2" >"$work/$2/.model"
    chown -R root:root "$work/$2"
    chmod -R u=rwX,go=rX "$work/$2"
    rm -rf "$dest"
    mv "$work/$2" "$dest"
    rm -rf "$work"
}
if [ "$REFLEXES" = yes ]; then
    say "Vosk models for the reflexes"
    LANGS=$(for g in "$HERE"/opt/characters/*/grammar/*.toml; do [ -e "$g" ] && basename "$g" .toml; done | sort -u)
    for l in $LANGS; do
        line=$(printf '%s\n' "$VOSK_MODELS" | awk -v l="$l" '$1 == l')
        if [ -z "$line" ]; then
            warn "no Vosk model known for '$l' (VOSK_MODELS in install.sh): no reflexes in that language"
            continue
        fi
        # shellcheck disable=SC2086
        ensure_model $line
    done
else
    note "reflexes: no models (--no-reflexes)"
fi

# ---------------------------------------------------------------- configuration files
toml_bool() { [ "$1" = yes ] && echo true || echo false; }
# fill TEMPLATE DEST OWNER-GROUP
fill() {
    if [ -e "$2" ]; then
        run cp -p "$2" "$2.prev"
    fi
    if [ -n "$DRY" ]; then
        printf '    + write %s (language %s, name "%s", url "%s", reflexes %s, web %s, airplay %s)\n' \
            "$2" "${LANGUAGE:-unset}" "$DEVICE_NAME" "${HA_URL:-unset}" "$(toml_bool "$REFLEXES")" "$(toml_bool "${WEB:-no}")" "$(toml_bool "${AIRPLAY:-no}")"
        return
    fi
    # Home Assistant's address, when it is an IPv4 literal, may connect before the QR link too (a
    # Wyoming integration added by hand)
    host=${HA_URL#*://}
    host=${host%%[:/]*}
    allowed=
    printf '%s' "$host" | grep -Eq '^[0-9]{1,3}(\.[0-9]{1,3}){3}$' && allowed="\"$host\""
    lang_set='' url_set=''
    [ -n "$LANGUAGE" ] || lang_set='#'
    [ -n "$HA_URL" ] || url_set='#'
    sed -e "s|@ALLOWED@|$allowed|g" -e "s|@LANG_SET@|$lang_set|g" -e "s|@URL_SET@|$url_set|g" \
        -e "s|@LANGUAGE@|${LANGUAGE:-en}|g" -e "s|@HA_URL@|${HA_URL:-http://192.0.2.10:8123}|g" \
        -e "s|@NAME@|$DEVICE_NAME|g" \
        -e "s|@REFLEXES@|$(toml_bool "$REFLEXES")|g" -e "s|@WEB@|$(toml_bool "${WEB:-no}")|g" \
        -e "s|@AIRPLAY@|$(toml_bool "${AIRPLAY:-no}")|g" "$1" >"$2.new"
    chown "root:$3" "$2.new"
    chmod 0640 "$2.new"
    mv -f "$2.new" "$2"
}
say "configuration in $ETC"
run install -d -m 0755 "$ETC"
if [ "$CONFIG_MODE" = new ]; then
    fill "$HERE/packaging/home-companion.toml.in" "$ETC/home-companion.toml" home-companion
    if [ ! -e "$ETC/brain.toml" ] || [ "$CONFIG_EXISTS" = yes ]; then
        fill "$HERE/packaging/brain.toml.in" "$ETC/brain.toml" home-companion-brain
    fi
else
    note "kept $ETC/home-companion.toml"
    if [ -e "$ETC/brain.toml" ]; then
        note "kept $ETC/brain.toml"
    else
        LANGUAGE=$(sed -n 's/^language *= *"\(.*\)"/\1/p' "$ETC/home-companion.toml" | head -n 1)
        HA_URL=$(sed -n 's/^url *= *"\(.*\)"/\1/p' "$ETC/home-companion.toml" | head -n 1)
        fill "$HERE/packaging/brain.toml.in" "$ETC/brain.toml" home-companion-brain
    fi
fi

# ---------------------------------------------------------------- systemd, udev, firewall
say "services, udev rule, firewall"
put "$HERE/packaging/home-companion.service" "$UNITS/home-companion.service" 0644
put "$HERE/packaging/home-companion-brain.service" "$UNITS/home-companion-brain.service" 0644
if [ "$WEB_AVAILABLE" = yes ]; then
    put "$HERE/packaging/home-companion-web.service" "$UNITS/home-companion-web.service" 0644
    # mesa-freedreno 26.3.0-2 fixes freedreno's GMEM loop: the browser no longer needs sysmem rendering
    if dpkg --compare-versions "$(pkg_version mesa-freedreno)" ge "$MESA_GMEM_FIXED" 2>/dev/null; then
        run mkdir -p "$UNITS/home-companion-web.service.d"
        if [ -n "$DRY" ]; then
            printf '    + write %s\n' "$WEB_MESA_DROPIN"
        else
            printf '# written by install.sh: mesa-freedreno %s has the GMEM fix\n[Service]\nEnvironment=FD_MESA_DEBUG=\n' \
                "$(pkg_version mesa-freedreno)" > "$WEB_MESA_DROPIN"
        fi
    elif [ -e "$WEB_MESA_DROPIN" ]; then
        run rm -f "$WEB_MESA_DROPIN"
    fi
elif have_unit home-companion-web.service; then
    run systemctl disable home-companion-web.service
    run rm -f "$UNITS/home-companion-web.service"
fi
put "$HERE/packaging/99-home-companion-backlight.rules" "$UDEV_RULE" 0644
run udevadm control --reload
run udevadm trigger -s backlight
if [ -d /etc/nftables.d ] && grep -qs 'nftables.d' /etc/nftables.conf; then
    if [ -e "$NFT_FILE" ] && cmp -s "$HERE/packaging/home-companion.nft" "$NFT_FILE"; then
        note "firewall: $NFT_FILE unchanged"
    else
        [ -e "$NFT_FILE" ] && run cp -p "$NFT_FILE" "$NFT_FILE.prev"
        put "$HERE/packaging/home-companion.nft" "$NFT_FILE" 0644
        if [ -n "$DRY" ]; then
            printf '    + nft -c -f /etc/nftables.conf && systemctl reload nftables\n'
        elif nft -c -f /etc/nftables.conf; then
            rm -f "$NFT_FILE.prev"
            systemctl -q is-active nftables && systemctl reload nftables
        else
            if [ -e "$NFT_FILE.prev" ]; then mv -f "$NFT_FILE.prev" "$NFT_FILE"; else rm -f "$NFT_FILE"; fi
            die "the firewall rejects $NFT_FILE (left as it was): see the nft error above"
        fi
    fi
else
    warn "this system's firewall does not include /etc/nftables.d/*.nft (thinksmart-view-debian before v0.3-rc2): open TCP 8124, 10700, 10701 (and AirPlay TCP 6010-6011, UDP 6010-6012) from the LAN by hand"
fi

# ---------------------------------------------------------------- libraries check
for b in home-companion home-companion-brain home-companion-reflex airplay/uxplay; do
    [ "$b" = airplay/uxplay ] && [ "$AIRPLAY" != yes ] && continue
    f=$PREFIX/$b
    [ -e "$f" ] || f=$HERE/opt/$b
    if LD_LIBRARY_PATH=$HERE/opt/lib ldd "$f" 2>/dev/null | grep -q 'not found'; then
        warn "$b: missing libraries: $(LD_LIBRARY_PATH=$HERE/opt/lib ldd "$f" | awk '/not found/ {print $1}' | tr '\n' ' ')"
    fi
done

# ---------------------------------------------------------------- start
run install -m 0644 "$HERE/VERSION" "$PREFIX/VERSION"
run systemctl daemon-reload
UNITS_ON="home-companion.service home-companion-brain.service"
[ "$WEB_AVAILABLE" = yes ] && UNITS_ON="$UNITS_ON home-companion-web.service"
# shellcheck disable=SC2086
run systemctl enable $UNITS_ON
if [ -n "$DRY" ] || systemctl -q is-active home-companion.service; then
    say "restarting the services"
    # home-companion-web is PartOf home-companion: it restarts with it
    run systemctl restart home-companion.service home-companion-brain.service
else
    say "starting the services"
    run systemctl start home-companion.service home-companion-brain.service
fi

# ---------------------------------------------------------------- next steps
IP=$(ip -4 -o route get 192.0.2.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p')
cat <<EOF

home-companion $BUNDLE_VERSION is $( [ -n "$DRY" ] && echo "ready to be installed (dry run)" || echo installed).

Next:
  1. On the device's screen: Altro -> Home Assistant shows a QR code. Open it with a phone logged in
     to Home Assistant and confirm: the device links itself and sets up the rest in Home Assistant
     (the Wyoming satellite, its tools over MCP, the conversation agent, the voice pipeline), then
     shows what it did. By hand instead: Settings -> Devices & services -> Add integration ->
     Wyoming Protocol, host ${IP:-<device address>}, port 10700.
  2. Say "Okay Alfred".

Configuration: $ETC/home-companion.toml (then systemctl restart home-companion)
Log:           journalctl -u home-companion -f
Remove:        $HERE/install.sh --uninstall [--purge]
EOF
