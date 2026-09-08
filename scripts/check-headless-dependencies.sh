#!/usr/bin/env bash
# Keep the self-host image and headless CI free of desktop credential backends.
set -euo pipefail
cd "$(dirname "$0")/.."

check_graph() {
    local label="$1"
    shift
    local graph
    graph=$(cargo tree --locked --target x86_64-unknown-linux-gnu --prefix none "$@")
    if [[ -z "$graph" ]]; then
        echo "$label returned an empty dependency graph" >&2
        return 1
    fi
    local forbidden
    forbidden=$(printf '%s\n' "$graph" | grep -E '^(keyring|secret-service|zbus|async-executor|async-io) v' || true)
    if [[ -n "$forbidden" ]]; then
        printf '%s includes desktop credential dependencies:\n%s\n' "$label" "$forbidden" >&2
        return 1
    fi
    echo "$label excludes desktop credential dependencies"
}

check_graph "Self-host image" -p tidebreak-cli -p tidebreak-supervised-agent \
    --no-default-features --features tidebreak-server/postgres
check_graph "Headless tests" --workspace --exclude tidebreak-desktop --no-default-features

# Desktop clients must keep the persistent backend and the server boot path.
for client in tidebreak-cli tidebreak-desktop; do
    graph=$(cargo tree --locked --target x86_64-unknown-linux-gnu -p "$client" \
        --prefix none --format '{p} {f}')
    if ! grep -Eq '^keyring v' <<<"$graph" || \
       ! grep -Eq '^tidebreak-server-core v.* keychain(,|$)' <<<"$graph"; then
        echo "$client must enable persistent keychain storage in the server" >&2
        exit 1
    fi
    echo "$client retains persistent keychain storage"
done
