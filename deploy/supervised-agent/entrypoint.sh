#!/bin/sh
# Start only inside Gateway's credential-free workload contract.
set -eu

: "${MODEL_GATEWAY_SANDBOX_ID:?Gateway must provide a sandbox identity}"
: "${MODEL_GATEWAY_SANDBOX_SUPERVISOR_ENDPOINT:?Gateway must provide its supervisor endpoint}"
: "${MODEL_GATEWAY_SANDBOX_GATEWAY_URL:?Gateway must provide its inference origin}"
: "${MODEL_GATEWAY_SANDBOX_PLACEHOLDER_TOKEN:?Gateway must provide its placeholder}"

if [ "$MODEL_GATEWAY_SANDBOX_PLACEHOLDER_TOKEN" != mg-sandbox-placeholder ]; then
    echo "Gateway's public workload placeholder is required." >&2
    exit 2
fi
case "${TIDEBREAK_AGENT_ENGINE:-claude_code}" in
    claude_code|codex) ;;
    *)
        echo "This image supports the pinned Claude Code and Codex engines." >&2
        exit 2
        ;;
esac
if [ -n "${GH_TOKEN:-}" ] && [ "$GH_TOKEN" != "$MODEL_GATEWAY_SANDBOX_PLACEHOLDER_TOKEN" ]; then
    echo "The GitHub CLI must use Gateway's public workload placeholder." >&2
    exit 2
fi
mkdir -p "$HOME"
exec /usr/local/bin/tidebreak-supervised-agent "$@"
