#!/usr/bin/env bash
#
# Remove generated build artifacts from inactive Git worktrees. By default this
# only reports candidates; pass --yes to delete. Directories with open files
# are always skipped, so a running Cargo, Tauri, or Vite process is not
# disturbed.
#
# Cleans both in-tree `target/` (default Cargo layout) and the per-worktree
# cache at ~/.cache/cargo-targets/<repo>/<worktree> used by the local cargo
# shim. The repo's `primary` cache (main checkout seed) is never removed.
#
#   scripts/clean-worktree-artifacts.sh
#   scripts/clean-worktree-artifacts.sh --worktree ../finished-task --yes
#   scripts/clean-worktree-artifacts.sh --worktree . --include-current --yes

set -euo pipefail

assume_yes=false
include_current=false
selected_worktrees=()
cargo_target_cache="${CARGO_TARGET_CACHE:-$HOME/.cache/cargo-targets}"

usage() {
  cat <<'EOF'
Usage: scripts/clean-worktree-artifacts.sh [--yes] [--worktree PATH] [--include-current]

Report generated Rust and desktop-UI artifacts in registered Git worktrees.
The current worktree is excluded unless --include-current is passed. --yes
requires at least one --worktree PATH and deletes only its generated directories
that have no open files. Never deletes the shared primary Cargo target cache.
EOF
}

# Map a worktree path to its external Cargo target dir, if the local shim
# layout applies. Prints nothing for the primary checkout (seed — keep it).
cargo_cache_target_for_worktree() {
  local worktree="$1" base rest repo wt
  base="$worktree"

  case "$base" in
  */orca/workspaces/*/*)
    rest="${base#*/orca/workspaces/}"
    repo="${rest%%/*}"
    wt="${rest#*/}"
    wt="${wt%%/*}"
    if [[ -n "$repo" && -n "$wt" && "$wt" != "$rest" && "$wt" != "primary" ]]; then
      printf '%s\n' "$cargo_target_cache/$repo/$wt"
      return 0
    fi
    ;;
  */.claude/worktrees/*)
    wt="$(basename "$base")"
    repo="$(basename "$(dirname "$(dirname "$(dirname "$base")")")")"
    printf '%s\n' "$cargo_target_cache/$repo/$wt"
    return 0
    ;;
  esac
  return 1
}

while (($#)); do
  case "$1" in
  --yes | -y) assume_yes=true ;;
  --worktree)
    [[ $# -ge 2 ]] || {
      echo "--worktree requires a path." >&2
      exit 2
    }
    selected_worktrees+=("$2")
    shift
    ;;
  --include-current) include_current=true ;;
  --help | -h)
    usage
    exit 0
    ;;
  *)
    usage >&2
    exit 2
    ;;
  esac
  shift
done

if $assume_yes && ((${#selected_worktrees[@]} == 0)); then
  echo "Refusing a bulk deletion: pass one or more --worktree PATH values." >&2
  exit 2
fi

command -v git >/dev/null || {
  echo "git is required." >&2
  exit 1
}
command -v lsof >/dev/null || {
  echo "lsof is required so active build outputs can be skipped safely." >&2
  exit 1
}

repo_root="$(git rev-parse --show-toplevel)"
current_worktree="$(cd "$repo_root" && pwd -P)"
worktree_list="$(mktemp)"
trap 'rm -f "$worktree_list"' EXIT
git -C "$repo_root" worktree list --porcelain |
  awk '/^worktree / { sub(/^worktree /, ""); print }' >"$worktree_list"

for index in "${!selected_worktrees[@]}"; do
  requested="${selected_worktrees[$index]}"
  [[ -d "$requested" ]] || {
    echo "Worktree does not exist: $requested" >&2
    exit 2
  }
  selected_worktrees[$index]="$(cd "$requested" && pwd -P)"
done

found=0
removable=0
skipped=0
matched=0

while IFS= read -r worktree; do
  [[ -d "$worktree" ]] || continue
  worktree="$(cd "$worktree" && pwd -P)"
  if [[ "$worktree" == "$current_worktree" && "$include_current" == false ]]; then
    continue
  fi

  if ((${#selected_worktrees[@]})); then
    selected=false
    for requested in "${selected_worktrees[@]}"; do
      [[ "$worktree" == "$requested" ]] || continue
      selected=true
      matched=$((matched + 1))
      break
    done
    $selected || continue
  fi

  artifacts=(
    "$worktree/target"
    "$worktree/crates/openwave-desktop/ui/node_modules"
    "$worktree/crates/openwave-desktop/ui/dist"
  )
  if cache_target="$(cargo_cache_target_for_worktree "$worktree")"; then
    artifacts+=("$cache_target")
  fi

  for artifact in "${artifacts[@]}"; do
    [[ -d "$artifact" && ! -L "$artifact" ]] || continue
    found=$((found + 1))
    size="$(du -sh "$artifact" 2>/dev/null | awk '{print $1}')"
    open_files="$(lsof +D "$artifact" 2>/dev/null || true)"
    # Spotlight and the file-system event daemon can index an otherwise idle
    # directory. Their handles do not make a build artifact unsafe to remove.
    open_files="$(printf '%s\n' "$open_files" | awk 'NR == 1 || ($1 != "mds" && $1 !~ /^mdworker/ && $1 != "StorageMa" && $1 != "fseventsd")')"

    if [[ -n "$open_files" ]]; then
      skipped=$((skipped + 1))
      echo "SKIP (in use)  $size  $artifact"
      printf '%s\n' "$open_files" | sed -n '2,4p' | sed 's/^/               /'
      continue
    fi

    removable=$((removable + 1))
    if $assume_yes; then
      echo "DELETE         $size  $artifact"
      find "$artifact" -depth -delete
    else
      echo "WOULD DELETE   $size  $artifact"
    fi
  done
done <"$worktree_list"

if ((${#selected_worktrees[@]})) && ((matched != ${#selected_worktrees[@]})); then
  echo "One or more --worktree paths are not registered in this repository." >&2
  exit 2
fi

if ((found == 0)); then
  echo "No generated worktree artifacts found."
elif ! $assume_yes; then
  printf '\n%s directory(s) can be removed; %s directory(s) are in use.\n' "$removable" "$skipped"
  echo "Re-run with --worktree PATH --yes to delete a finished worktree's artifacts."
else
  printf '\nRemoved %s directory(s); skipped %s directory(s) in use.\n' "$removable" "$skipped"
fi
