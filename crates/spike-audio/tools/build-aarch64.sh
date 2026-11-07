#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Cross-compiles spike-audio for the device and stages what to copy into
#   crates/spike-audio/out/aarch64/
# Two binaries: `spike-audio` (no WebRTC, no extra library) and `spike-audio-webrtc`
# (WebRTC through tools/build-webrtc-aarch64.sh, needs libwebrtc-audio-processing-2.so.1
# next to it: the binary carries an rpath of $ORIGIN).
#
# usage: tools/build-aarch64.sh [--no-webrtc]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
root="$(cd "$here/../.." && pwd)"
stage="$here/out/aarch64"
prefix="$here/third_party/wap-aarch64"
triple=aarch64-unknown-linux-gnu
target_dir="${CARGO_TARGET_DIR:-$root/target}"

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export PKG_CONFIG_SYSROOT_DIR=/ PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig

mkdir -p "$stage"
cd "$root"
cargo build -p spike-audio --release --target "$triple"
aarch64-linux-gnu-strip -o "$stage/spike-audio" "$target_dir/$triple/release/spike-audio"

if [[ "${1:-}" != "--no-webrtc" ]]; then
    if [[ ! -f "$prefix/lib/pkgconfig/webrtc-audio-processing-2.pc" ]]; then
        "$here/tools/build-webrtc-aarch64.sh"
    fi
    # A separate target dir: different features and RUSTFLAGS must not evict the plain build.
    PKG_CONFIG_PATH="$prefix/lib/pkgconfig:$PKG_CONFIG_PATH" \
    CXXFLAGS_aarch64_unknown_linux_gnu="-I$prefix/include" \
    BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu="-I$prefix/include" \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-Wl,-rpath,\$ORIGIN" \
        cargo build -p spike-audio --release --target "$triple" --features webrtc-system \
        --target-dir "$target_dir/webrtc-system"
    aarch64-linux-gnu-strip -o "$stage/spike-audio-webrtc" "$target_dir/webrtc-system/$triple/release/spike-audio"
    aarch64-linux-gnu-strip -o "$stage/libwebrtc-audio-processing-2.so.1" "$prefix/lib/libwebrtc-audio-processing-2.so.1"
fi

echo "staged for the device in $stage:"
ls -l "$stage"
file "$stage"/*
