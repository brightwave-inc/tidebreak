#!/usr/bin/env bash
# Compile only. The caller owns signing, fixture consent, and supervised trials.
set -euo pipefail
helper_dir="$(cd "$(dirname "$0")" && pwd)"
if [[ $# != 1 || "$1" != /* ]]; then
  echo "Usage: build-targeted-trial.sh /absolute/output-binary" >&2
  exit 2
fi
swiftc -D HELPER_TESTS -parse-as-library \
  "$helper_dir"/Sources/tidebreak-cu-helper/*.swift \
  "$helper_dir"/Tests/tidebreak-cu-helperTests/TargetedInputTrial.swift \
  -o "$1"
