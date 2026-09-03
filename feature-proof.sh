#!/usr/bin/env bash
# Sandbox-only helper (not committed): prints, for each dependency that must
# unify, the feature list cargo resolves in each build selection plus its md5.
set -u
cd /workspace/tidebreak
EDGES="features"
if [ "${1:-}" = "--no-dev" ]; then EDGES="features,normal,build"; fi
DEPS=(tokio serde_json url tidebreak-core tidebreak-server)
SELECTIONS=("-p tidebreak-server" "-p tidebreak-cli" "-p tidebreak-core" "-p tidebreak-desktop" "--workspace")
for dep in "${DEPS[@]}"; do
  echo "=== $dep ==="
  for sel in "${SELECTIONS[@]}"; do
    if [ "$dep" = tidebreak-server ] && { [ "$sel" = "-p tidebreak-core" ]; }; then
      echo "  [$sel] (server not in graph)"; continue
    fi
    out=$(cargo tree -e "$EDGES" -i "$dep" $sel 2>&1)
    if [ $? -ne 0 ]; then echo "  [$sel] ERROR: $(echo "$out" | head -2 | tr '\n' ' ')"; continue; fi
    # The first line is the dep itself; feature lines are "<name> feature \"x\"".
    feats=$(echo "$out" | grep -oE "${dep} feature \"[^\"]+\"" | sort -u)
    md5=$(echo "$feats" | md5sum | cut -d' ' -f1)
    echo "  [$sel] md5=$md5"
    echo "$feats" | sed 's/^/      /'
  done
done
