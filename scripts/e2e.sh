#!/usr/bin/env bash
#
# Run the end-to-end lane: build the debug self-host server and the desktop
# renderer, then drive both through Chromium with Playwright.
#
# The lane boots one machine per flow, with the scripted model and coding
# engine in place of real ones, so nothing leaves this computer. It runs
# PostgreSQL and an S3 gateway in throwaway containers, which is why it needs
# Docker, and it removes them when it finishes.
#
#   scripts/e2e.sh                        # every flow
#   scripts/e2e.sh tests/commit.spec.ts   # one flow
#   scripts/e2e.sh --headed               # watch the browser
#
# Every argument is forwarded to `playwright test`. Set TIDEBREAK_E2E_BINARY
# or TIDEBREAK_E2E_UI_DIST to reuse a server or renderer you already built.
# Traces for failed flows land in e2e/test-results.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(dirname "$script_dir")"
ui_dir="$repo_root/crates/tidebreak-desktop/ui"
e2e_dir="$repo_root/e2e"

missing=()
for tool in cargo docker pnpm; do
  command -v "$tool" >/dev/null || missing+=("$tool")
done
if ((${#missing[@]} > 0)); then
  echo "Missing prerequisites: ${missing[*]}" >&2
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  echo "Docker is installed but its daemon is not answering. Start Docker and try again." >&2
  exit 1
fi

if [[ -z "${TIDEBREAK_E2E_BINARY:-}" ]]; then
  echo "==> Building the debug self-host server"
  (cd "$repo_root" && cargo build --locked -p tidebreak-cli --features tidebreak-server/postgres)
fi

if [[ -z "${TIDEBREAK_E2E_UI_DIST:-}" ]]; then
  echo "==> Building the desktop renderer"
  CI=true pnpm --dir "$ui_dir" install --frozen-lockfile
  pnpm --dir "$ui_dir" build
fi

echo "==> Installing the end-to-end tools"
(cd "$e2e_dir" && CI=true pnpm install --frozen-lockfile)
(cd "$e2e_dir" && pnpm exec playwright install --only-shell chromium)

echo "==> Running the flows"
cd "$e2e_dir"
exec pnpm exec playwright test "$@"
