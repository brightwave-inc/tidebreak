#!/usr/bin/env bash

# Cargo runner for macOS (wired up in `.cargo/config.toml`): re-sign the
# freshly built binary with a stable code-signing identity, then launch it.
#
# Why: Tidebreak stores secrets in the macOS login keychain, and keychain
# access approvals are tied to the binary's code signature. Dev builds are
# only ad-hoc signed by the linker, so every rebuild produces a new
# signature and the "Tidebreak wants to use your confidential information"
# prompt returns — "Always Allow" can never stick. Signing with a real,
# stable identity gives the binary a stable designated requirement, so one
# approval per binary persists across rebuilds.
#
# Every binary is signed with the same fixed identifier (`tidebreak-dev`)
# rather than codesign's default (the file name). The keychain ACL matches on
# the designated requirement — identifier + certificate — so with a shared
# identifier one "Always Allow" covers every dev binary, including test
# executables, whose hashed file names would otherwise produce a fresh
# designated requirement (and a fresh prompt) on every rebuild.
#
# That only holds for a certificate carrying a team identifier. macOS builds
# the ACL entry from a requirement it can re-evaluate — identifier plus the
# leaf's team — and any later build satisfying it is let through. A
# self-signed certificate has no team identifier, so there is nothing stable
# to match on and the approval is pinned to the binary's cdhash instead: the
# next rebuild invalidates it and the prompt returns. A team-identified
# identity therefore wins over the local-only one, even though using one means
# development touches a distribution key that it otherwise would not.
#
# Identity, in order of preference:
#   1. $TIDEBREAK_DEV_SIGNING_IDENTITY (set to opt out with an empty value)
#   2. the first "Apple Development" identity — team-identified, and the one
#      meant for local builds
#   3. the first "Developer ID Application" identity — also team-identified
#   4. an "tidebreak-dev" certificate already in a searchable keychain
#   5. a local-only "tidebreak-dev" identity bootstrapped in its own keychain,
#      which stops the per-rebuild prompt for nothing else on the list
#
# Switching between identities re-homes nothing: credentials stored under the
# previous one keep prompting until `cargo run -p tidebreak-cli --
# rehome-secrets` rewrites them.

set -euo pipefail

bin="$1"
shift
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

find_identity() {
  local identities
  identities="$(security find-identity -v -p codesigning 2>/dev/null)" || return 0
  local pattern
  for pattern in \
    '"\(Apple Development: [^"]*\)"' \
    '"\(Developer ID Application: [^"]*\)"' \
    '"\(tidebreak-dev\)"'; do
    local match
    match="$(sed -n "s/.*${pattern}.*/\\1/p" <<<"$identities" | head -n 1)"
    if [[ -n "$match" ]]; then
      printf '%s' "$match"
      return 0
    fi
  done
  return 0
}

if [[ -n "${TIDEBREAK_DEV_SIGNING_IDENTITY+x}" ]]; then
  identity="$TIDEBREAK_DEV_SIGNING_IDENTITY"
else
  identity="$(find_identity)"
  if [[ -z "$identity" ]]; then
    # Nothing team-identified to sign with. The bootstrapped certificate is
    # self-signed and untrusted, so `security find-identity -p codesigning`
    # never lists it even though codesign can use it; setting it up (or
    # confirming it is already there) is what makes it available.
    if "$script_dir/setup-macos-dev-signing.sh"; then
      identity="tidebreak-dev"
    else
      echo "warning: could not set up durable macOS dev signing; running unsigned" >&2
    fi
  fi
fi

identifier="tidebreak-dev"

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
