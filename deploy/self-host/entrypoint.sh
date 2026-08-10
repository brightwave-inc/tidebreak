#!/bin/sh
# Boot the OpenWave daemon and publish it on a fixed container port.
#
# Why this script exists at all: the server binds its listener to
# `127.0.0.1:0` unconditionally (`bind_inner` in crates/openwave-server/src/
# lib.rs) and prints the resulting ephemeral address on stdout. That is right
# for the desktop profile — one client on one machine — but inside a container
# it means the API is reachable from nowhere, because loopback is the
# container's own network namespace and the port is not known in advance.
#
# So this script reads the address the daemon announces and bridges a fixed
# port to it. It is a packaging bridge, not a design: when the server grows a
# configurable bind address, delete the bridge, drop socat from the image, and
# let the daemon listen on the published port directly.
#
# It also keeps the daemon's per-launch bearer token out of the container log.
# That token authenticates nobody under the self-host profile (see the `auth`
# module docs), but a secret-shaped string in `docker logs` invites someone to
# treat it as one.

set -eu

PORT="${OPENWAVE_LISTEN_PORT:-8080}"
# How long to wait for the daemon to announce its address. First boot runs
# database migrations, so this is generous.
BOOT_TIMEOUT_SECS="${OPENWAVE_BOOT_TIMEOUT_SECS:-180}"

runtime="$(mktemp -d)"
announced="${runtime}/announced-port"
stream="${runtime}/daemon-output"
mkfifo "$stream"

cleanup() {
    [ -n "${server_pid:-}" ] && kill "$server_pid" 2>/dev/null || true
    [ -n "${bridge_pid:-}" ] && kill "$bridge_pid" 2>/dev/null || true
    rm -rf "$runtime"
}
trap cleanup EXIT
trap 'exit 143' TERM INT

openwave serve >"$stream" 2>&1 &
server_pid=$!

# Drain the daemon's output for the life of the process: a full pipe would
# block it. Along the way, record the announced port and suppress the token.
(
    while IFS= read -r line; do
        case "$line" in
            "openwave: token "*)
                printf 'openwave: per-launch token withheld from the container log\n'
                continue
                ;;
            "openwave: listening on http://"*)
                printf '%s\n' "${line##*:}" >"$announced"
                ;;
        esac
        printf '%s\n' "$line"
    done <"$stream"
) &

waited=0
while [ ! -s "$announced" ]; do
    if ! kill -0 "$server_pid" 2>/dev/null; then
        echo "openwave: the daemon exited before it announced an address" >&2
        wait "$server_pid"
        exit 1
    fi
    if [ "$waited" -ge "$BOOT_TIMEOUT_SECS" ]; then
        echo "openwave: no address announced within ${BOOT_TIMEOUT_SECS}s" >&2
        exit 1
    fi
    sleep 1
    waited=$((waited + 1))
done

internal_port="$(cat "$announced")"
echo "openwave: publishing 0.0.0.0:${PORT} -> 127.0.0.1:${internal_port}"

socat "TCP-LISTEN:${PORT},fork,reuseaddr" "TCP:127.0.0.1:${internal_port}" &
bridge_pid=$!

# Exit as soon as either half stops, so the container's restart policy sees a
# half-dead process as dead.
while kill -0 "$server_pid" 2>/dev/null && kill -0 "$bridge_pid" 2>/dev/null; do
    sleep 2
done

echo "openwave: daemon or bridge exited; shutting down" >&2
exit 1
