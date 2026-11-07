#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Fallback A for the aarch64 WebRTC build: the `bundled` feature of
# webrtc-audio-processing-sys 2.1 runs meson without a cross file, so its objects come out
# x86_64 and the aarch64 link fails. This script builds the same upstream sources
# (webrtc-audio-processing 2.1, shipped inside the -sys crate) for aarch64 with a meson
# cross file, as one shared library with abseil linked in statically, into
#   crates/spike-audio/third_party/wap-aarch64/
# The crate's non-bundled mode (cargo feature `webrtc-system`) then finds it with pkg-config.
#
# Needs: meson, ninja, aarch64-linux-gnu-gcc/g++, network once (meson fetches abseil).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tp="$here/third_party"
src="$tp/wap-src"
build="$tp/wap-build-aarch64"
prefix="$tp/wap-aarch64"
cross="$tp/aarch64-cross.ini"
jobs="${CARGO_BUILD_JOBS:-2}"

mkdir -p "$tp"
if [[ ! -f "$src/meson.build" ]]; then
    registry="${CARGO_HOME:-$HOME/.cargo}/registry/src"
    crate="$(find "$registry" -mindepth 2 -maxdepth 2 -type d -name 'webrtc-audio-processing-sys-2.1.*' | sort | tail -n 1)"
    if [[ -z "$crate" ]]; then
        echo "webrtc-audio-processing-sys 2.1 not in $registry: run 'cargo fetch' in the workspace first" >&2
        exit 1
    fi
    echo "copying sources from $crate"
    cp -a "$crate/webrtc-audio-processing" "$src"
fi

cat > "$cross" <<'EOF'
[binaries]
c = 'aarch64-linux-gnu-gcc'
cpp = 'aarch64-linux-gnu-g++'
ar = 'aarch64-linux-gnu-ar'
strip = 'aarch64-linux-gnu-strip'
pkg-config = 'pkg-config'

[properties]
pkg_config_libdir = '/usr/lib/aarch64-linux-gnu/pkgconfig'

[host_machine]
system = 'linux'
cpu_family = 'aarch64'
cpu = 'cortex-a53'
endian = 'little'
EOF

if [[ ! -f "$build/build.ninja" ]]; then
    meson setup "$build" "$src" \
        --cross-file "$cross" \
        --prefix "$prefix" \
        --libdir lib \
        --buildtype release \
        -Ddefault_library=shared \
        --force-fallback-for=abseil-cpp \
        -Dabseil-cpp:default_library=static
fi
ninja -C "$build" -j "$jobs"
ninja -C "$build" -j "$jobs" install

lib="$(find "$prefix/lib" -maxdepth 1 -name 'libwebrtc-audio-processing-2.so*' -type f | head -n 1)"
echo
echo "built: $lib"
file "$lib"
cat <<EOF

Build the spike against it (from the workspace root):

  export PATH=\$HOME/.cargo/bin:\$PATH CARGO_BUILD_JOBS=2
  PKG_CONFIG_SYSROOT_DIR=/ PKG_CONFIG_ALLOW_CROSS=1 \\
  PKG_CONFIG_PATH=$prefix/lib/pkgconfig:/usr/lib/aarch64-linux-gnu/pkgconfig \\
  CXXFLAGS_aarch64_unknown_linux_gnu="-I$prefix/include" \\
  BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu="-I$prefix/include" \\
  CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS='-C link-arg=-Wl,-rpath,\$ORIGIN' \\
  cargo build -p spike-audio --release --target aarch64-unknown-linux-gnu --features webrtc-system

Copy $(basename "$lib") (and its .so.N symlink name) next to the binary on the device.
EOF
