#!/usr/bin/env bash
# Materialize the self-host Docker context from Git, never from the caller's
# working tree. Docker uploads a local context before evaluating COPY, so a
# .dockerignore alone cannot make arbitrary untracked files safe.
set -euo pipefail

destination="${1:?usage: stage-self-host-build-context.sh DESTINATION [REVISION]}"
revision="${2:-HEAD}"
root="$(git rev-parse --show-toplevel)"

mkdir -p "$destination"
git -C "$root" archive --format=tar "$revision" | tar -x -C "$destination"
