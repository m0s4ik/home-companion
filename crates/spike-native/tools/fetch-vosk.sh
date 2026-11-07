#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Download the official prebuilt libvosk 0.3.45 (Apache-2.0) for x86_64 and aarch64 into
# third_party/vosk/<arch>/ (libvosk.so + vosk_api.h) and link the Italian model into models/vosk-it.
# The model directory defaults to ~/prove-vosk/vosk-model-small-it-0.22 (override with VOSK_MODEL).
# Idempotent: nothing is downloaded when the libraries are already there.
set -euo pipefail

VERSION=0.3.45
BASE=https://github.com/alphacep/vosk-api/releases/download/v$VERSION
declare -A SHA=(
    [x86_64]=bbdc8ed85c43979f6443142889770ea95cbfbc56cffb5c5dcd73afa875c5fbb2
    [aarch64]=45e95d37755deb07568e79497d7feba8c03aee5a9e071df29961aa023fd94541
)

here=$(cd "$(dirname "$0")/.." && pwd)
dst="$here/third_party/vosk"
mkdir -p "$dst"

for arch in x86_64 aarch64; do
    name=vosk-linux-$arch-$VERSION
    want=${SHA[$arch]}
    if [ -f "$dst/$arch/.fetched-$want" ]; then
        echo "ok: $name already fetched"
    else
        rm -rf "${dst:?}/$arch" "${dst:?}/$name" "$dst/$name.zip"
        curl -fsSL -o "$dst/$name.zip" "$BASE/$name.zip"
        got=$(sha256sum "$dst/$name.zip" | cut -d' ' -f1)
        if [ "$got" != "$want" ]; then
            echo "error: $name.zip sha256 $got, expected $want" >&2
            exit 1
        fi
        unzip -q "$dst/$name.zip" -d "$dst"
        mkdir -p "$dst/$arch"
        mv "$dst/$name/libvosk.so" "$dst/$name/vosk_api.h" "$dst/$arch/"
        rm -rf "${dst:?}/$name" "$dst/$name.zip"
        touch "$dst/$arch/.fetched-$want"
    fi
    echo "sha256 $want  $name.zip"
done

model=${VOSK_MODEL:-$HOME/prove-vosk/vosk-model-small-it-0.22}
mkdir -p "$here/models"
if [ -d "$model" ]; then
    ln -sfn "$model" "$here/models/vosk-it"
    echo "model: models/vosk-it -> $model"
else
    echo "warning: Vosk model not found at $model (set VOSK_MODEL); models/vosk-it not created" >&2
fi
