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
command -v node >/dev/null || missing+=("Node.js — https://nodejs.org/")
command -v pnpm >/dev/null || missing+=("pnpm — https://pnpm.io/installation")
cargo tauri --version >/dev/null 2>&1 ||
  missing+=("Tauri CLI 2 — cargo install tauri-cli --version \"^2\"")

if ((${#missing[@]})); then
  echo "Missing prerequisites:" >&2
  printf '  - %s\n' "${missing[@]}" >&2
  exit 1
fi

assert_dev_port_available() {
  command -v lsof >/dev/null || return 0
  local dev_server_pids
  dev_server_pids="$(lsof -nP -iTCP:1420 -sTCP:LISTEN -t 2>/dev/null | paste -sd, - || true)"
  [[ -z "$dev_server_pids" ]] || {
    echo "Tidebreak's dev port 1420 is already in use by PID(s): $dev_server_pids" >&2
    echo "Stop the existing dev server, then run scripts/dev.sh again." >&2
    return 1
  }
}

assert_dev_port_available

echo "==> Installing UI dependencies"
pnpm --dir "$ui_dir" install

echo "==> Preparing desktop sidecars"
# Resolve the same target directory that Cargo will use. This matters when a
# local Cargo wrapper assigns one target directory per worktree.
cargo_metadata="$(
  cd "$repo_root"
  cargo metadata --format-version 1 --no-deps
)"
cargo_target_dir="$(
  printf '%s' "$cargo_metadata" |
    node -e 'process.stdout.write(JSON.parse(require("node:fs").readFileSync(0, "utf8")).target_directory)'
)"
export CARGO_TARGET_DIR="$cargo_target_dir"
node "$desktop_dir/scripts/prepare-sidecar.mjs"

echo "==> Starting the desktop app"
# Keep sidecar compilation outside Tauri's frontend wait loop. Direct
# `cargo tauri dev` still uses the repository's standard preparation hook.
assert_dev_port_available
dev_config='{"build":{"beforeDevCommand":{"script":"pnpm dev","cwd":"ui"}}}'
cd "$desktop_dir"
exec cargo tauri dev --config "$dev_config" "$@"
