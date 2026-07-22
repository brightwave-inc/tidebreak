#!/usr/bin/env bash

set -euo pipefail

: "${APPLE_SIGNING_IDENTITY:?APPLE_SIGNING_IDENTITY is required}"

pdfium_dylib="crates/openwave-desktop/resources/pdfium/libpdfium.dylib"
[[ -f "$pdfium_dylib" ]] || {
  echo "The staged PDFium runtime is missing: $pdfium_dylib" >&2
  exit 1
}

# Tauri copies arbitrary bundle resources without signing nested Mach-O files.
# Sign PDFium before the bundling phase so its Developer ID signature survives
# the resource copy and Apple can notarize the enclosing application.
codesign \
  --force \
  --options runtime \
  --timestamp \
  --sign "$APPLE_SIGNING_IDENTITY" \
  "$pdfium_dylib"

codesign --verify --strict --verbose=2 "$pdfium_dylib"

signature_details="$(codesign --display --verbose=4 "$pdfium_dylib" 2>&1)"
[[ "$signature_details" == *"Authority=$APPLE_SIGNING_IDENTITY"* ]] || {
  echo "PDFium was not signed by the expected Developer ID identity." >&2
  exit 1
}
[[ "$signature_details" == *"Timestamp="* ]] || {
  echo "PDFium signature does not contain a secure timestamp." >&2
  exit 1
}

printf '%s\n' "$signature_details"
