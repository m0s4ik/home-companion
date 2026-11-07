#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# UxPlay 1.73.7 with our patch (uxplay-1.73.7-home-companion.patch: option -hc, see crates/home-companion/src/airplay.rs) for
# arm64 Debian 13. Output: <out>/uxplay, stripped, to install as /opt/home-companion/airplay/uxplay.
#
#   tools/airplay/build-uxplay.sh [out]          # out defaults to ./out/uxplay
#
# Two ways, chosen by the machine it runs on:
# - on arm64 Debian 13 (the device itself, a container, a Mac with `docker run --platform linux/arm64
#   debian:trixie`): native build. Build dependencies:
#     apt-get install --no-install-recommends git ca-certificates cmake make gcc g++ pkg-config \
#       libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libplist-dev libssl-dev \
#       libavahi-compat-libdnssd-dev
# - on amd64 Debian 13 with the arm64 multiarch libraries (the same machine that cross builds home-companion): cross build.
#     dpkg --add-architecture arm64 && apt-get update
#     apt-get install --no-install-recommends git ca-certificates cmake make pkg-config \
#       gcc-aarch64-linux-gnu g++-aarch64-linux-gnu \
#       libgstreamer1.0-dev:arm64 libgstreamer-plugins-base1.0-dev:arm64 libplist-dev:arm64 \
#       libssl-dev:arm64 libavahi-compat-libdnssd-dev:arm64
#
# What is left out: X11 (-DNO_X11_DEPS=ON; home-companion draws the mirrored frames itself) and D-Bus (only used by
# UxPlay's -scrsv screensaver inhibitor). The binary needs at run time: libgstreamer1.0-0,
# libgstreamer-plugins-base1.0-0, libplist-2.0-4, libavahi-compat-libdnssd1, libssl3t64 (and the GStreamer
# plugins listed in packaging/README.md).
set -eu

VERSION=v1.73.7
HERE=$(cd "$(dirname "$0")" && pwd)
PATCH=$HERE/uxplay-1.73.7-home-companion.patch
OUT=$(mkdir -p "${1:-out/uxplay}" && cd "${1:-out/uxplay}" && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

git -c advice.detachedHead=false clone -q --depth 1 -b "$VERSION" https://github.com/FDH2/UxPlay.git "$WORK/src"
cd "$WORK/src"
git apply "$PATCH"
# D-Bus is optional upstream but picked up whenever its headers exist: never needed here.
sed -i 's/^\(\s*\)pkg_check_modules(DBUS dbus-1>=1.4.12)/\1set(DBUS_FOUND OFF)/' CMakeLists.txt
grep -q 'set(DBUS_FOUND OFF)' CMakeLists.txt

set -- -DCMAKE_BUILD_TYPE=Release -DNO_X11_DEPS=ON -DNO_MARCH_NATIVE=ON
STRIP=strip
case "$(uname -m)" in
aarch64 | arm64) ;;
*)
    # Cross: the arm64 multiarch libraries of this Debian, found through their own pkg-config files.
    export PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig
    export PKG_CONFIG_SYSROOT_DIR=/
    set -- "$@" -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=aarch64 \
        -DCMAKE_C_COMPILER=aarch64-linux-gnu-gcc -DCMAKE_CXX_COMPILER=aarch64-linux-gnu-g++ \
        -DCMAKE_LIBRARY_ARCHITECTURE=aarch64-linux-gnu \
        -DCMAKE_FIND_ROOT_PATH_MODE_PROGRAM=NEVER
    STRIP=aarch64-linux-gnu-strip
    ;;
esac

cmake -S . -B build "$@" >build.log 2>&1 || { cat build.log; exit 1; }
make -C build -j"$(nproc)" uxplay >>build.log 2>&1 || { tail -40 build.log; exit 1; }
"$STRIP" -o "$OUT/uxplay" build/uxplay
ls -l "$OUT/uxplay"
