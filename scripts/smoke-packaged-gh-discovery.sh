#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 /path/to/Tidebreak.app" >&2
  exit 2
fi

app_path="$1"
info_plist="$app_path/Contents/Info.plist"
[[ -d "$app_path" && -f "$info_plist" ]] || {
  echo "Packaged app is missing: $app_path" >&2
  exit 1
}

executable="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$info_plist")"
identifier="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$info_plist")"
app_executable="$app_path/Contents/MacOS/$executable"
[[ -x "$app_executable" ]] || {
  echo "Packaged app executable is missing: $app_executable" >&2
  exit 1
}

finder_path="/usr/bin:/bin:/usr/sbin:/sbin"
if PATH="$finder_path" command -v gh >/dev/null 2>&1; then
  echo "The Finder-style PATH unexpectedly contains gh." >&2
  exit 1
fi

smoke_root="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/tidebreak-packaged-gh.XXXXXX")"
profile_home="$smoke_root/home"
profile_data="$profile_home/Library/Application Support/$identifier"
shell_config="$smoke_root/zsh"
fake_bin="$smoke_root/login-bin"
fake_log="$smoke_root/gh-config/invocations.log"
app_log="$smoke_root/app.log"
response="$smoke_root/repositories.json"
app_pid=""

cleanup() {
  local status=$?
  trap - EXIT INT TERM
  if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then
    kill -TERM "$app_pid" 2>/dev/null || true
    for _ in {1..50}; do
      kill -0 "$app_pid" 2>/dev/null || break
      sleep 0.1
    done
    if kill -0 "$app_pid" 2>/dev/null; then
      kill -KILL "$app_pid" 2>/dev/null || true
    fi
  fi
  if [[ -n "$app_pid" ]]; then
    wait "$app_pid" 2>/dev/null || true
  fi
  rm -rf -- "$smoke_root"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p \
  "$profile_home" \
  "$shell_config" \
  "$fake_bin" \
  "$smoke_root/gh-config" \
  "$smoke_root/tmp"

cat > "$fake_bin/gh" <<'FAKE_GH'
#!/bin/sh
set -eu

: "${GH_CONFIG_DIR:?}"
printf '%s\n' "$*" >> "$GH_CONFIG_DIR/invocations.log"

if [ "$#" -eq 4 ] \
  && [ "$1" = auth ] \
  && [ "$2" = status ] \
  && [ "$3" = --json ] \
  && [ "$4" = hosts ]; then
  printf '%s\n' '{"hosts":{"github.com":[{"state":"failure","active":true,"login":""}]}}'
  exit 0
fi

printf 'Unexpected gh command: %s\n' "$*" >&2
exit 97
FAKE_GH
chmod 755 "$fake_bin/gh"

cat > "$shell_config/.zshrc" <<'ZSHRC'
export PATH="$GH_SMOKE_LOGIN_BIN:$PATH"
ZSHRC

/usr/bin/env -i \
  HOME="$profile_home" \
  CFFIXED_USER_HOME="$profile_home" \
  USER="${USER:-runner}" \
  LOGNAME="${LOGNAME:-${USER:-runner}}" \
  SHELL=/bin/zsh \
  ZDOTDIR="$shell_config" \
  PATH="$finder_path" \
  TMPDIR="$smoke_root/tmp" \
  GH_CONFIG_DIR="$smoke_root/gh-config" \
  GH_SMOKE_LOGIN_BIN="$fake_bin" \
  "$app_executable" > "$app_log" 2>&1 &
app_pid=$!

listen_path="$profile_data/listen.json"
for _ in {1..120}; do
  if [[ -s "$listen_path" ]]; then
    break
  fi
  if ! kill -0 "$app_pid" 2>/dev/null; then
    echo "The packaged app exited before publishing listen.json." >&2
    sed -n '1,200p' "$app_log" >&2
    exit 1
  fi
  sleep 0.5
done

[[ -s "$listen_path" ]] || {
  echo "The packaged app did not publish $listen_path." >&2
  sed -n '1,200p' "$app_log" >&2
  exit 1
}

base_url="$(jq -er .base_url "$listen_path")"
token="$(jq -er .token "$listen_path")"
/usr/bin/curl \
  --fail \
  --silent \
  --show-error \
  --max-time 15 \
  --header "Authorization: Bearer $token" \
  "$base_url/code/delivery/repositories" > "$response"

if ! jq -e '.capability.found == true' "$response" >/dev/null; then
  echo "The packaged app did not discover gh through the login shell." >&2
  jq '.capability' "$response" >&2 || cat "$response" >&2
  sed -n '1,200p' "$app_log" >&2
  exit 1
fi
if ! jq -e '.capability.authenticated == false' "$response" >/dev/null; then
  echo "The packaged app did not report the fake gh account as signed out." >&2
  jq '.capability' "$response" >&2 || cat "$response" >&2
  sed -n '1,200p' "$app_log" >&2
  exit 1
fi

[[ -s "$fake_log" ]] || {
  echo "The packaged app reported gh without invoking the login-shell binary." >&2
  echo "Discovery must probe the login shell before /opt/homebrew/bin/gh or /usr/local/bin/gh." >&2
  exit 1
}
if grep -Fvx 'auth status --json hosts' "$fake_log" >/dev/null; then
  echo "The packaged app ran an unexpected gh command:" >&2
  cat "$fake_log" >&2
  exit 1
fi

echo "Packaged GitHub CLI discovery passed with a Finder-style PATH."
