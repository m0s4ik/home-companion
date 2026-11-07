#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Every source file of the M1 crates, the CI and the tools starts with the two SPDX lines
# (`// ` for Rust, Slint and C, `# ` for TOML, shell, YAML and Python; a shebang may come first).
# Third-party and data files are not checked. Exit 1 with the files that lack them.
set -eu
cd "$(dirname "$0")/.."
missing=0
files=$(git ls-files -co --exclude-standard -- crates/home-companion crates/wyoming crates/audio crates/wakeword \
    crates/webview-proto crates/webview-fake crates/webview-viewer web/protocol-test.c web/home-companion-web \
    .github tools \
    | grep -E '\.(rs|toml|sh|yml|yaml|slint|py|c|h)$' \
    | grep -vE '(^|/)third_party/|(^|/)out/' || true)
for f in $files; do
    [ -f "$f" ] || continue
    head=$(head -n 4 "$f")
    if ! printf '%s\n' "$head" | grep -qE '^(//|#) SPDX-License-Identifier: GPL-3.0-or-later$' ||
       ! printf '%s\n' "$head" | grep -qE '^(//|#) SPDX-FileCopyrightText: 2026 Andrea Mattera$'; then
        echo "missing SPDX header: $f"
        missing=1
    fi
done
[ $missing -eq 0 ] && echo "check-spdx: ok ($(printf '%s\n' "$files" | grep -c .) files)"
exit $missing
