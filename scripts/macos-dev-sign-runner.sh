#!/usr/bin/env bash

# Cargo runner for macOS (wired up in `.cargo/config.toml`): re-sign the
# freshly built binary with a stable code-signing identity, then launch it.
#
# Why: OpenWave stores secrets in the macOS login keychain, and keychain
# access approvals are tied to the binary's code signature. Dev builds are
# only ad-hoc signed by the linker, so every rebuild produces a new
# signature and the "OpenWave wants to use your confidential information"
# prompt returns — "Always Allow" can never stick. Signing with a real,
# stable identity gives the binary a stable designated requirement, so one
# approval per binary persists across rebuilds.
#
# Every binary is signed with the same fixed identifier (`openwave-dev`)
# rather than codesign's default (the file name). The keychain ACL matches on
# the designated requirement — identifier + certificate — so with a shared
# identifier one "Always Allow" covers every dev binary, including test
# executables, whose hashed file names would otherwise produce a fresh
# designated requirement (and a fresh prompt) on every rebuild.
#
# Identity, in order of preference:
#   1. $OPENWAVE_DEV_SIGNING_IDENTITY (set to opt out with an empty value)
#   2. an "openwave-dev" self-signed certificate, if one exists
#   3. the first "Apple Development" identity in the keychain
#   4. a local-only "openwave-dev" identity bootstrapped in its own keychain

set -euo pipefail

bin="$1"
shift
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

find_identity() {
  local identities
  identities="$(security find-identity -v -p codesigning 2>/dev/null)" || return 0
  local pattern
  for pattern in '"\(openwave-dev\)"' '"\(Apple Development: [^"]*\)"'; do
    local match
    match="$(sed -n "s/.*${pattern}.*/\\1/p" <<<"$identities" | head -n 1)"
    if [[ -n "$match" ]]; then
      printf '%s' "$match"
      return 0
    fi
  done
}

if [[ -n "${OPENWAVE_DEV_SIGNING_IDENTITY+x}" ]]; then
  identity="$OPENWAVE_DEV_SIGNING_IDENTITY"
elif "$script_dir/setup-macos-dev-signing.sh" --existing-only; then
  # The bootstrapped certificate is intentionally self-signed and untrusted,
  # so `security find-identity -p codesigning` does not list it even though
  # codesign can use it. Check its dedicated keychain first.
  identity="openwave-dev"
else
  identity="$(find_identity)"
  if [[ -z "$identity" ]]; then
    if "$script_dir/setup-macos-dev-signing.sh"; then
      identity="openwave-dev"
    else
      echo "warning: could not set up durable macOS dev signing; running unsigned" >&2
    fi
  fi
fi

identifier="openwave-dev"

if [[ -n "$identity" ]]; then
  # Unchanged binaries keep their signature from the previous run; re-signing
  # only when the identity or identifier differs leaves them untouched.
  current="$(codesign --display --verbose=2 "$bin" 2>&1 || true)"
  if [[ "$current" != *"Authority=$identity"* || "$current" != *"Identifier=$identifier"* ]]; then
    codesign --force --sign "$identity" --identifier "$identifier" "$bin" 2>/dev/null ||
      echo "warning: codesign with '$identity' failed; running unsigned" >&2
  fi
fi

exec "$bin" "$@"
