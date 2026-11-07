#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# No private data in the repository: private (RFC 1918) and link-local addresses, MAC
# addresses, tokens, `.local` host names. Documentation uses 192.0.2.x and example.home.
# Checks the tracked text files (and new untracked ones), not target/, Cargo.lock or binaries.
# If .private-patterns exists (git-ignored, one extended regex per line: the real names of
# the house), its patterns are checked too. Exit 1 with the lines found.
set -eu
cd "$(dirname "$0")/.."
pattern='\b10\.[0-9]+\.[0-9]+\.[0-9]+\b|\b192\.168\.|\b172\.(1[6-9]|2[0-9]|3[01])\.[0-9]+\.[0-9]+|\b169\.254\.|\b([0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}\b|token\s*=\s*"[^"]{8,}|Bearer [A-Za-z0-9._-]{16,}|[A-Za-z0-9-]\.local\b'
files=$(git ls-files -co --exclude-standard | grep -vE '^(target/|Cargo\.lock$)|(^|/)third_party/|(^|/)out/|^tools/check-private\.sh$' || true)
found=0
check() {
    # -I: skip binary files.
    out=$(printf '%s\n' "$files" | tr '\n' '\0' | xargs -0 grep -nIE "$1" 2>/dev/null || true)
    if [ -n "$out" ]; then
        printf '%s\n' "$out"
        found=1
    fi
}
check "$pattern"
if [ -f .private-patterns ]; then
    while IFS= read -r p; do
        [ -n "$p" ] && check "$p"
    done < .private-patterns
fi
if [ $found -eq 0 ]; then
    echo "check-private: ok ($(printf '%s\n' "$files" | grep -c .) files)"
fi
exit $found
