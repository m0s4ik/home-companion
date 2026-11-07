#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Builds the release bundle home-companion-<version>-arm64.tar.gz: the binaries cross-built for arm64 and
# stripped, the character, the locales, the systemd units, the firewall and udev files, install.sh and the
# licences, in one tree that install.sh installs on the device (packaging/README.md).
#
#   tools/release/build-bundle.sh <version> [out-dir]        # out-dir defaults to out/release
#
# Runs on amd64 Debian 13 with the arm64 multiarch libraries, Rust's aarch64-unknown-linux-gnu target and
# aarch64-linux-gnu-gcc, like the device builds. Environment:
#   WEB_HELPER=<file>  the web helper home-companion-web (web/, C) already built for arm64: it needs
#                      libwpewebkit-2.0-dev 2.54, which an amd64 cross setup does not have; built here with
#                      `make -C web/home-companion-web` when its pkg-config file is found
#   UXPLAY=<file>      UxPlay already built by tools/airplay/build-uxplay.sh (else it is built now)
#   JOBS=<n>           cargo's jobs (default 6)
# The Vosk language models are not in the bundle (~50-90 MB each): install.sh downloads them.
set -eu

VERSION=${1:?usage: build-bundle.sh <version> [out-dir]}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT=$(mkdir -p "${2:-$ROOT/out/release}" && cd "${2:-$ROOT/out/release}" && pwd)
NAME=home-companion-$VERSION-arm64
STAGE=$OUT/$NAME
TARGET=aarch64-unknown-linux-gnu
REL=$ROOT/target/$TARGET/release
STRIP=aarch64-linux-gnu-strip

case "$VERSION" in *[!0-9A-Za-z.~+-]*) echo "version: letters, digits and . ~ + - only" >&2; exit 2 ;; esac
cd "$ROOT"
COMMIT=$(git rev-parse --short HEAD)
DIRTY=
if git status --porcelain --untracked-files=no | grep -q .; then DIRTY=+dirty; fi

export PATH="$HOME/.cargo/bin:$PATH" CARGO_BUILD_JOBS="${JOBS:-6}"
export PKG_CONFIG_SYSROOT_DIR=/ PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig PKG_CONFIG_ALLOW_CROSS=1

echo "== binaries ($TARGET)"
[ -f crates/spike-native/third_party/vosk/aarch64/libvosk.so ] || crates/spike-native/tools/fetch-vosk.sh
cargo build -q --release --target $TARGET -p home-companion --no-default-features --features fbdev,gpu
cargo build -q --release --target $TARGET -p brain
cargo build -q --release --target $TARGET -p reflex --features vosk --bin home-companion-reflex
git checkout -q Cargo.lock 2>/dev/null || true

rm -rf "$STAGE" "$OUT/$NAME.tar.gz"
mkdir -p "$STAGE/opt/lib" "$STAGE/opt/airplay" "$STAGE/packaging" "$STAGE/tools" "$STAGE/licenses" "$STAGE/source"
for b in home-companion home-companion-brain home-companion-reflex; do
    $STRIP -o "$STAGE/opt/$b" "$REL/$b"
done
install -m 0755 crates/spike-native/third_party/vosk/aarch64/libvosk.so "$STAGE/opt/lib/libvosk.so"

echo "== web helper"
if [ -n "${WEB_HELPER:-}" ]; then
    install -m 0755 "$WEB_HELPER" "$STAGE/opt/home-companion-web"
    WEB_FROM="prebuilt ($(basename "$WEB_HELPER"), sha256 $(sha256sum "$WEB_HELPER" | cut -c1-16))"
elif PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig pkg-config --exists wpe-webkit-2.0; then
    make -C web/home-companion-web CC=aarch64-linux-gnu-gcc \
        PKG_CONFIG="env PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig pkg-config" >/dev/null
    $STRIP -o "$STAGE/opt/home-companion-web" web/home-companion-web/home-companion-web
    WEB_FROM="built from web/home-companion-web"
else
    echo "no libwpewebkit-2.0-dev for arm64 here: set WEB_HELPER=<prebuilt home-companion-web>" >&2
    exit 1
fi
file "$STAGE/opt/home-companion-web" | grep -q 'ARM aarch64' || { echo "$WEB_HELPER: not an arm64 binary" >&2; exit 1; }

echo "== UxPlay"
if [ -n "${UXPLAY:-}" ]; then
    install -m 0755 "$UXPLAY" "$STAGE/opt/airplay/uxplay"
else
    tools/airplay/build-uxplay.sh "$OUT/uxplay" >/dev/null
    install -m 0755 "$OUT/uxplay/uxplay" "$STAGE/opt/airplay/uxplay"
fi
file "$STAGE/opt/airplay/uxplay" | grep -q 'ARM aarch64' || { echo "uxplay: not an arm64 binary" >&2; exit 1; }

echo "== data, packaging, docs"
cp -R characters "$STAGE/opt/characters"
cp -R locales "$STAGE/opt/locales"
cp packaging/home-companion.service packaging/home-companion-brain.service packaging/home-companion-web.service \
    packaging/home-companion.nft packaging/99-home-companion-backlight.rules \
    packaging/home-companion.toml.in packaging/brain.toml.in "$STAGE/packaging/"
install -m 0755 packaging/install.sh "$STAGE/install.sh"
install -m 0755 tools/slim-vosk-model.py "$STAGE/tools/"
cp packaging/INSTALL.md "$STAGE/INSTALL.md"
cp LICENSE "$STAGE/LICENSE"
# What the binaries contain or the bundle carries, and under which licence
cp /usr/share/common-licenses/Apache-2.0 "$STAGE/licenses/Apache-2.0.txt"
cp third_party/femtovg/LICENSE-MIT "$STAGE/licenses/femtovg-LICENSE-MIT"
cp crates/wakeword/THIRD_PARTY.md "$STAGE/licenses/wakeword-THIRD_PARTY.md"
cp third_party/femtovg/femtovg-0.27.0-rotation.patch "$STAGE/source/" 2>/dev/null || cp third_party/femtovg-0.27.0-rotation.patch "$STAGE/source/"
cp tools/airplay/uxplay-1.73.7-home-companion.patch tools/airplay/build-uxplay.sh "$STAGE/source/"
{
    echo "# Third-party software in home-companion $VERSION"
    echo
    echo "home-companion itself is GPL-3.0-or-later (LICENSE); the character Alfred is CC0-1.0 (opt/characters/alfred),"
    echo "except the wake and stop word models, whose licences are next to them (opt/characters/alfred/wakeword/LICENSE,"
    echo "opt/characters/alfred/stopword/LICENSE-stop and README.md: \"basta\" and \"stop_alfred\" were trained with"
    echo "synthetic voices that include Piper voices under non-commercial licences)."
    echo
    echo "- opt/lib/libvosk.so: Vosk API 0.3.45 (Alpha Cephei), Apache-2.0 (licenses/Apache-2.0.txt), unmodified"
    echo "  official build. The language models install.sh downloads (vosk-model-small-*) are Apache-2.0 too."
    echo "- opt/airplay/uxplay: UxPlay 1.73.7 (https://github.com/FDH2/UxPlay, tag v1.73.7), GPL-3.0 (LICENSE), with"
    echo "  source/uxplay-1.73.7-home-companion.patch, built by source/build-uxplay.sh."
    echo "- TensorFlow Lite Micro, microfrontend, FlatBuffers (Apache-2.0) and KISS FFT (BSD-3-Clause) in
  opt/home-companion (the wake word): licenses/wakeword-THIRD_PARTY.md.
- femtovg 0.27.0 (in opt/home-companion): MIT OR Apache-2.0, with source/femtovg-0.27.0-rotation.patch."
    echo "- opt/home-companion-web links the system's WPE WebKit (LGPL-2.1/BSD) dynamically; nothing of it is in the bundle."
    echo
    echo "## Rust crates compiled into the binaries (name version | licence)"
    echo
    for spec in "home-companion --no-default-features --features fbdev,gpu" "brain" "reflex --features vosk"; do
        # shellcheck disable=SC2086
        cargo tree -p $spec --target $TARGET -e normal --prefix none -f '{p}|{l}' 2>/dev/null
    done | sed -e 's/ (\*)$//' -e 's/ (\/[^)]*)//' -e 's/ (proc-macro)//' | sort -u | sed 's/^/- /'
} >"$STAGE/THIRD-PARTY.md"
printf 'version=%s\ncommit=%s%s\nbuilt=%s\nweb_helper=%s\n' "$VERSION" "$COMMIT" "$DIRTY" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "$WEB_FROM" | sed "s|$ROOT/||; s|$HOME|~|" >"$STAGE/VERSION"

# Nothing private in the bundle's text files
if grep -rIl -E '192\.168\.|10\.100\.' "$STAGE" >/dev/null; then
    echo "private-looking addresses in:" >&2
    grep -rIl -E '192\.168\.|10\.100\.' "$STAGE" >&2
    exit 1
fi

(cd "$STAGE" && find . -type f -print | LC_ALL=C sort | xargs sha256sum >"$OUT/SHA256SUMS.tmp")
mv "$OUT/SHA256SUMS.tmp" "$STAGE/SHA256SUMS"
tar -C "$OUT" --owner=0 --group=0 --numeric-owner --sort=name -czf "$OUT/$NAME.tar.gz" "$NAME"
ls -l "$OUT/$NAME.tar.gz"
du -sh "$STAGE"
