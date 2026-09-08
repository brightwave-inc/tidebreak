#!/usr/bin/env bash
# Static Swift validation for the fixture. Real type-checking requires macOS;
# on Linux this is intentionally a no-op that keeps CI honest about what ran.
set -euo pipefail

SOURCE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/main.swift"

if command -v swiftc >/dev/null 2>&1; then
    swiftc -typecheck -target arm64-apple-macosx13.0 "$SOURCE"
else
    echo "computer-use-fixture: no swiftc; native Swift validation must run on macOS" >&2
    exit 0
fi
