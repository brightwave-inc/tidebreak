#!/usr/bin/env bash
# Run no-input helper regressions with Swift Command Line Tools or Xcode.
set -euo pipefail
helper_dir="$(cd "$(dirname "$0")" && pwd)"
test_dir="$(mktemp -d "${TMPDIR:-/tmp}/tidebreak-helper-tests.XXXXXX")"
trap 'rm -rf "$test_dir"' EXIT
swiftc -D HELPER_TESTS -parse-as-library \
  "$helper_dir"/Sources/tidebreak-cu-helper/*.swift \
  "$helper_dir"/Tests/tidebreak-cu-helperTests/HelperTests.swift \
  -o "$test_dir/helper-tests"
"$test_dir/helper-tests"
