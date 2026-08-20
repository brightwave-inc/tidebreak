#!/usr/bin/env bash
#
# Install the desktop UI dependencies and start Storybook.
#
# Storybook lives in crates/tidebreak-desktop/ui, which is not the directory
# you are usually in, and skipping the install leaves it on a stale
# dependency tree. This does both, from anywhere in the repo.
#
#   scripts/storybook.sh # the usual Storybook dev server
#
# Every argument is forwarded to `pnpm storybook`.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(dirname "$script_dir")"
ui_dir="$repo_root/crates/tidebreak-desktop/ui"

if ! command -v pnpm >/dev/null; then
  echo "Missing prerequisites:" >&2
  echo "  - pnpm — https://pnpm.io/installation" >&2
  exit 1
fi

echo "==> Installing UI dependencies"
pnpm --dir "$ui_dir" install

echo "==> Starting Storybook"
exec pnpm --dir "$ui_dir" storybook "$@"
