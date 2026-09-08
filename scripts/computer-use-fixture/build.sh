#!/usr/bin/env bash
# Reproducible build for the Computer Use Fixture app bundle.
# Requires macOS with Xcode Command Line Tools (swiftc + codesign).
#
# Usage:
#   scripts/computer-use-fixture/build.sh /tmp/tidebreak-cu-fixture
set -euo pipefail

SOURCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${1:?usage: scripts/computer-use-fixture/build.sh <output-directory>}"
MAIN_SOURCE="$SOURCE_DIR/main.swift"
INFO_SOURCE="$SOURCE_DIR/Info.plist"
SCRIPT_SOURCE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/build.sh"

if [[ "$OUT_DIR" == "/" || "$OUT_DIR" == "$HOME" || "$OUT_DIR" == "$SOURCE_DIR" ]]; then
    echo "computer-use-fixture: refusing a broad build directory" >&2
    exit 2
fi

APP_DIR="$OUT_DIR/ComputerUseFixture.app"
CONTENTS="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS/MacOS"
RESOURCES_DIR="$CONTENTS/Resources"
MARKER="$OUT_DIR/BUILD.repro.md"
BINARY="$MACOS_DIR/ComputerUseFixture"

if [[ ! -f "$MAIN_SOURCE" || ! -f "$INFO_SOURCE" ]]; then
    echo "computer-use-fixture: sources are missing in $SOURCE_DIR" >&2
    exit 2
fi

# Static preflight. On macOS this is a real Swift type check; on Linux it
# reports that native validation must run on macOS.
if ! command -v swiftc >/dev/null 2>&1; then
    echo "computer-use-fixture: swiftc is required (macOS or a Swift toolchain)" >&2
    exit 2
fi

SOURCE_HASH="$(shasum -a 256 "$MAIN_SOURCE" "$INFO_SOURCE" "$SCRIPT_SOURCE" | shasum -a 256 | awk '{print $1}')"

if [[ -x "$BINARY" && -f "$MARKER" ]] && grep -q "source-hash: $SOURCE_HASH" "$MARKER" 2>/dev/null; then
    echo "computer-use-fixture: up to date at $APP_DIR"
    echo "open $APP_DIR --args --fixture-dir $OUT_DIR/state --run-id <fresh-uuid>"
    exit 0
fi

rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

swiftc -O -whole-module-optimization "$MAIN_SOURCE" -o "$BINARY"
cp "$INFO_SOURCE" "$RESOURCES_DIR/Info.plist"
printf 'APPL????' > "$CONTENTS/PkgInfo"

if command -v codesign >/dev/null 2>&1; then
    codesign --force --deep --sign - "$APP_DIR" >/dev/null 2>&1 || true
fi

cat > "$MARKER" <<EOF
# ComputerUseFixture reproducible build

- app: $APP_DIR
- bundle id: dev.tidebreak.ComputerUseFixture
- sources: $MAIN_SOURCE, $INFO_SOURCE
- source-hash: $SOURCE_HASH
- commands:

\`\`\`sh
swiftc -O -whole-module-optimization "$MAIN_SOURCE" -o "$BINARY"
cp "$INFO_SOURCE" "$RESOURCES_DIR/Info.plist"
codesign --force --deep --sign - "$APP_DIR"
\`\`\`

Run from a checkout at commit \`$(git -C "$SOURCE_DIR/.." rev-parse HEAD 2>/dev/null || echo unknown)\`.
EOF

echo "computer-use-fixture: built $APP_DIR"
echo "open $APP_DIR --args --fixture-dir $OUT_DIR/state --run-id <fresh-uuid>"
