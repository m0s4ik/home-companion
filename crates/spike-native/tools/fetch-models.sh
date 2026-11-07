#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Download the official microWakeWord "okay nabu" v2 model (Apache-2.0, not committed) into models/.
# Idempotent: files already present with the expected sha256 are not downloaded again.
set -euo pipefail

COMMIT=40ff33f57f8fc6ad71a75ef085abbf742495b225   # esphome/micro-wake-word-models, 2026-11-03
BASE=https://raw.githubusercontent.com/esphome/micro-wake-word-models/$COMMIT/models/v2

here=$(cd "$(dirname "$0")/.." && pwd)
models="$here/models"
mkdir -p "$models/v2"

# fetch <url> <destination> <expected sha256>
fetch() {
    local url=$1 dst=$2 want=$3 got
    if [ -f "$dst" ] && [ "$(sha256sum "$dst" | cut -d' ' -f1)" = "$want" ]; then
        echo "ok: $(basename "$dst") already present"
    else
        curl -fsSL -o "$dst.part" "$url"
        mv "$dst.part" "$dst"
    fi
    got=$(sha256sum "$dst" | cut -d' ' -f1)
    echo "sha256 $got  ${dst#"$here"/}"
    if [ "$got" != "$want" ]; then
        echo "error: sha256 mismatch for $dst (expected $want)" >&2
        exit 1
    fi
}

fetch "$BASE/okay_nabu.json" "$models/v2/okay_nabu.json" \
    6dd65604f70fe5ea9d1af73a7bf239529d1fbabc363807f45d2b22ce464ddbed
fetch "$BASE/okay_nabu.tflite" "$models/okay_nabu.tflite" \
    0689abe1912a95a3318a0d8cb2e67bad0cbcfe3e24dd6e050c75debddfb6f891
