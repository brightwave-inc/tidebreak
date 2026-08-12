#!/usr/bin/env bash

# Create the local-only signing identity used when a macOS contributor has no
# team-identified code-signing identity at all — see the preference list in
# macos-dev-sign-runner.sh.
#
# The identity lives in a dedicated keychain rather than the login keychain, so
# Tidebreak can unlock it without asking for the login password. Its private key
# has no value outside this machine.
#
# Being self-signed, the certificate carries no team identifier, so a keychain
# ACL granted to a binary it signed is pinned to that binary's cdhash and does
# not survive a rebuild. It gives local binaries a stable identity for
# everything else, but it cannot make credential approvals stick — which is why
# the runner reaches for it last.

set -euo pipefail

state_dir="${TIDEBREAK_DEV_SIGNING_DIR:-${HOME}/Library/Application Support/Tidebreak/dev-signing}"
keychain="$state_dir/tidebreak-dev.keychain-db"
password_file="$state_dir/keychain-password"
lock_dir="$state_dir/setup.lock"

umask 077
mkdir -p "$state_dir"

keychain_is_ready() {
  [[ -f "$keychain" && -s "$password_file" ]] || return 1
  local keychain_password
  keychain_password="$(<"$password_file")"
  security unlock-keychain -p "$keychain_password" "$keychain" >/dev/null 2>&1 &&
    security find-certificate -c tidebreak-dev "$keychain" >/dev/null 2>&1 &&
    security find-key -l tidebreak-dev -t private -s "$keychain" >/dev/null 2>&1
}

ensure_searchable() {
  local existing_keychains=()
  local found=false
  local line
  local existing_keychain
  while IFS= read -r line; do
    # `security list-keychains` indents and quotes every path.
    existing_keychain="${line#*\"}"
    existing_keychain="${existing_keychain%\"*}"
    [[ -n "$existing_keychain" ]] || continue
    existing_keychains+=("$existing_keychain")
    [[ "$existing_keychain" == "$keychain" ]] && found=true
  done < <(security list-keychains -d user)

  if [[ "$found" == false ]]; then
    security list-keychains -d user -s "$keychain" "${existing_keychains[@]}"
  fi
}

if keychain_is_ready; then
  ensure_searchable
  exit 0
fi

acquired_lock=false
for _ in {1..200}; do
  if mkdir "$lock_dir" 2>/dev/null; then
    acquired_lock=true
    break
  fi
  if keychain_is_ready; then
    ensure_searchable
    exit 0
  fi
  sleep 0.05
done

if [[ "$acquired_lock" == false ]]; then
  echo "warning: timed out waiting for macOS dev-signing setup" >&2
  exit 1
fi

temp_dir=""
cleanup() {
  [[ -z "$temp_dir" ]] || rm -r "$temp_dir"
  rmdir "$lock_dir" 2>/dev/null || true
}
trap cleanup EXIT

# Another runner may have completed setup immediately before this one acquired
# the lock.
if keychain_is_ready; then
  ensure_searchable
  exit 0
fi

if [[ -e "$keychain" || -e "$password_file" ]]; then
  echo "warning: incomplete Tidebreak dev-signing state in '$state_dir'" >&2
  echo "warning: remove that directory to let Tidebreak recreate it" >&2
  exit 1
fi

temp_dir="$(mktemp -d "$state_dir/identity.XXXXXX")"
keychain_password="$(/usr/bin/openssl rand -hex 32)"
printf '%s\n' "$keychain_password" >"$password_file"

/usr/bin/openssl req \
  -x509 \
  -newkey rsa:2048 \
  -sha256 \
  -nodes \
  -days 3650 \
  -subj "/CN=tidebreak-dev" \
  -addext "basicConstraints=critical,CA:FALSE" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning" \
  -keyout "$temp_dir/key.pem" \
  -out "$temp_dir/cert.pem" \
  >/dev/null 2>&1

/usr/bin/openssl pkcs12 \
  -export \
  -inkey "$temp_dir/key.pem" \
  -in "$temp_dir/cert.pem" \
  -name tidebreak-dev \
  -passout "pass:$keychain_password" \
  -out "$temp_dir/identity.p12"

security create-keychain -p "$keychain_password" "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$temp_dir/identity.p12" \
  -k "$keychain" \
  -P "$keychain_password" \
  -A \
  >/dev/null
# Keep parallel Cargo runners from racing a keychain that one of them has
# already re-locked. It still locks after six hours of inactivity.
security set-keychain-settings -u -t 21600 "$keychain"
ensure_searchable
