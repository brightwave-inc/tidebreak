#!/usr/bin/env bash
#
# Install the desktop UI dependencies and open the app in a native window.
#
# Running the desktop app takes two commands from a directory that is not the
# one you are usually in, and forgetting the first leaves Vite serving a stale
# dependency tree. This does both, from anywhere in the repo.
#
#   scripts/dev.sh # the usual dev build
#
# Every argument is forwarded to `cargo tauri dev`.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(dirname "$script_dir")"
desktop_dir="$repo_root/crates/tidebreak-desktop"
ui_dir="$desktop_dir/ui"

missing=()
command -v pnpm >/dev/null || missing+=("pnpm — https://pnpm.io/installation")
cargo tauri --version >/dev/null 2>&1 ||
  missing+=("Tauri CLI 2 — cargo install tauri-cli --version \"^2\"")

if ((${#missing[@]})); then
  echo "Missing prerequisites:" >&2
  printf '  - %s\n' "${missing[@]}" >&2
  exit 1
fi

echo "==> Installing UI dependencies"
pnpm --dir "$ui_dir" install

echo "==> Starting the desktop app"
# tauri.conf.json lives here, and its before-dev hook stages the broker sidecar
# and starts Vite.
cd "$desktop_dir"
exec cargo tauri dev "$@"
