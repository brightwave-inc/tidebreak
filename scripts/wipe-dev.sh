#!/usr/bin/env bash
#
# Wipe the dev build's local state so the next `scripts/dev.sh` run starts
# from a fresh profile.
#
# Dev builds keep every store separate from an installed release: the debug
# identifier override keys the app-data dir (`io.brightwave.tidebreak.dev`),
# secrets live under the `tidebreak.dev` keychain service, and on macOS the
# unbundled debug binary's WebView data lands under its process name
# (`tidebreak-desktop`) rather than a bundle identifier. This deletes exactly
# those dev stores and never touches the release profile.
#
#   scripts/wipe-dev.sh        # list what would be deleted and ask first
#   scripts/wipe-dev.sh --yes  # no prompt

set -euo pipefail

assume_yes=false
[[ "${1:-}" == "--yes" || "${1:-}" == "-y" ]] && assume_yes=true

dev_id="io.brightwave.tidebreak.dev"
dev_keychain_service="tidebreak.dev"
# The unbundled debug binary's name, which keys its WebView storage on macOS.
dev_process="tidebreak-desktop"

if pgrep -x "$dev_process" >/dev/null 2>&1; then
  echo "A dev build ($dev_process) is running; quit it first." >&2
  exit 1
fi

case "$(uname -s)" in
Darwin)
  targets=(
    "$HOME/Library/Application Support/$dev_id"
    "$HOME/Library/Caches/$dev_id"
    "$HOME/Library/WebKit/$dev_id"
    "$HOME/Library/Caches/$dev_process"
    "$HOME/Library/WebKit/$dev_process"
    "$HOME/Library/HTTPStorages/$dev_process"
    "$HOME/Library/Preferences/$dev_process.plist"
    "$HOME/Library/Saved Application State/$dev_process.savedState"
  )
  ;;
Linux)
  targets=(
    "${XDG_DATA_HOME:-$HOME/.local/share}/$dev_id"
    "${XDG_CONFIG_HOME:-$HOME/.config}/$dev_id"
    "${XDG_CACHE_HOME:-$HOME/.cache}/$dev_id"
  )
  ;;
*)
  echo "Unsupported platform: $(uname -s)" >&2
  exit 1
  ;;
esac

existing=()
for target in "${targets[@]}"; do
  [[ -e "$target" ]] && existing+=("$target")
done

if ((${#existing[@]} == 0)); then
  echo "No dev-profile files found."
else
  echo "Will delete:"
  printf '  %s\n' "${existing[@]}"
fi
echo "Will also remove every '$dev_keychain_service' secret-store entry."

if ! $assume_yes; then
  read -r -p "Proceed? [y/N] " reply
  [[ "$reply" == y || "$reply" == Y ]] || {
    echo "Aborted."
    exit 1
  }
fi

# macOS ships bash 3.2, where expanding an empty array trips `set -u`.
for target in ${existing[@]+"${existing[@]}"}; do
  rm -rf "$target"
done

case "$(uname -s)" in
Darwin)
  deleted=0
  while security delete-generic-password -s "$dev_keychain_service" \
    >/dev/null 2>&1; do
    deleted=$((deleted + 1))
  done
  echo "Removed $deleted keychain item(s)."
  # Drop the cached preferences domain along with the plist deleted above.
  defaults delete "$dev_process" >/dev/null 2>&1 || true
  ;;
Linux)
  # The keyring crate stores Secret Service entries with a `service`
  # attribute; `secret-tool clear` deletes every match.
  if command -v secret-tool >/dev/null 2>&1; then
    secret-tool clear service "$dev_keychain_service" || true
    echo "Cleared '$dev_keychain_service' Secret Service entries."
  else
    echo "secret-tool not found; remove '$dev_keychain_service' entries" \
      "with your secret manager." >&2
  fi
  ;;
esac

echo "Dev profile wiped."
