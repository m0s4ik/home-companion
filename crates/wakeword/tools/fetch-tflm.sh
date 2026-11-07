#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Fetch TensorFlow Lite Micro and the microfrontend C library at pinned commits.
#   third_party/tflite-micro   shallow clone of tensorflow/tflite-micro
#   third_party/tensorflow     sparse, shallow checkout of tensorflow/tensorflow (microfrontend only)
#   third_party/deps           flatbuffers and kissfft at the versions TFLM's Makefile pins
# Nothing downloaded is executed: the list of TFLM sources is decided by build.rs, not by
# create_tflm_tree.py (see README, "Why not create_tflm_tree.py").
# Idempotent: a second run does no network work. Needs git, curl, unzip, md5sum, sha256sum.
set -euo pipefail

TFLM_COMMIT=964e43d7ee3e82fe6abb6075b7f50506eafce79b   # tensorflow/tflite-micro, 2026-11-03
TF_COMMIT=e166dba74d0ff24174457522e3bf5655a117ab40     # tensorflow/tensorflow, 2026-11-03
FRONTEND_PATH=tensorflow/lite/experimental/microfrontend/lib

here=$(cd "$(dirname "$0")/.." && pwd)
tp="$here/third_party"
mkdir -p "$tp" "$tp/deps"

# Shallow fetch of one commit into $1 from $2; optional sparse path $4.
fetch_commit() {
    local dir=$1 url=$2 commit=$3 sparse=${4:-}
    if [ -f "$dir/.fetched-$commit" ]; then
        echo "ok: $(basename "$dir") already at $commit"
        return
    fi
    rm -rf "$dir"
    git init -q "$dir"
    git -C "$dir" remote add origin "$url"
    if [ -n "$sparse" ]; then
        git -C "$dir" sparse-checkout set --no-cone "/$sparse/"
        git -C "$dir" fetch -q --depth 1 --filter=blob:none origin "$commit"
    else
        git -C "$dir" fetch -q --depth 1 origin "$commit"
    fi
    git -C "$dir" -c advice.detachedHead=false checkout -q FETCH_HEAD
    touch "$dir/.fetched-$commit"
}

# Download a zip into deps/$1, check the md5 that TFLM pins, print its sha256, strip the top directory.
fetch_zip() {
    local name=$1 url=$2 md5=$3 dir="$tp/deps/$1" zip="$tp/deps/$1.zip"
    if [ -f "$dir/.fetched-$md5" ]; then
        echo "ok: $name already fetched"
        return
    fi
    rm -rf "$dir" "$dir.unpacked" "$zip"
    curl -fsSL -o "$zip" "$url"
    if [ "$(md5sum "$zip" | cut -d' ' -f1)" != "$md5" ]; then
        echo "error: $name md5 mismatch (expected $md5)" >&2
        exit 1
    fi
    echo "sha256 $(sha256sum "$zip" | cut -d' ' -f1)  $name.zip"
    unzip -q "$zip" -d "$dir.unpacked"
    mv "$dir.unpacked"/* "$dir"
    rmdir "$dir.unpacked"
    rm "$zip"
    touch "$dir/.fetched-$md5"
}

fetch_commit "$tp/tflite-micro" https://github.com/tensorflow/tflite-micro.git "$TFLM_COMMIT"
fetch_commit "$tp/tensorflow" https://github.com/tensorflow/tensorflow.git "$TF_COMMIT" "$FRONTEND_PATH"

# Third-party archives: the same URLs and md5 sums as the pinned TFLM tree
# (tensorflow/lite/micro/tools/make/download_dependencies.py). gemmlowp and ruy are no longer used by TFLM.
fetch_zip flatbuffers https://github.com/google/flatbuffers/archive/refs/tags/v25.9.23.zip \
    023eca1e211d64007124420cd6be29c7
fetch_zip kissfft https://github.com/mborgerding/kissfft/archive/refs/tags/131.2.0.zip \
    c1c39842615fdbe0c318f035b8322372

echo "tflite-micro commit: $(git -C "$tp/tflite-micro" rev-parse HEAD)"
echo "tensorflow commit:   $(git -C "$tp/tensorflow" rev-parse HEAD)"
echo "microfrontend:       $(find "$tp/tensorflow/$FRONTEND_PATH" -name '*.c' | wc -l) C files"
