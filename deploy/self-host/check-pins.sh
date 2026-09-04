#!/usr/bin/env bash
# Checks every download pin in deploy/self-host/Dockerfile against the
# publisher's own checksum list, so a pin that was typed rather than copied
# fails here instead of in an image build nobody runs before a release.
#
# Covered: rustup-init (static.rust-lang.org publishes a .sha256 beside each
# installer), Go (the go.dev download index), Node (SHASUMS256.txt per
# release), and gh (the release's checksums file). Debian packages pin by
# version and are verified by apt against the snapshot's signed index.
set -euo pipefail

dockerfile=${1:-$(dirname "$0")/Dockerfile}
failures=0

value() {
  # The first assignment of `name=<value>` in the Dockerfile, trailing
  # semicolons and backslashes stripped.
  grep -oE "\b$1=[^ ;\\\\]+" "$dockerfile" | head -1 | cut -d= -f2
}

pins_for() {
  # Every `<var>=<sha256>` assignment in the Dockerfile, in file order.
  grep -oE "\b$1=[0-9a-f]{64}" "$dockerfile" | cut -d= -f2
}

check() {
  local what=$1 expected=$2 actual=$3
  if [ "$expected" = "$actual" ]; then
    echo "ok   $what $actual"
  else
    echo "FAIL $what: Dockerfile pins $actual, publisher says ${expected:-<none>}" >&2
    failures=$((failures + 1))
  fi
}

# rustup: the Dockerfile lists amd64 then arm64.
rustup_version=$(value rustup_version)
read -r -a rustup_pins <<<"$(pins_for rustup_sha256 | tr '\n' ' ')"
i=0
for triple in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
  published=$(curl -fsSL "https://static.rust-lang.org/rustup/archive/${rustup_version}/${triple}/rustup-init.sha256" | cut -c1-64)
  check "rustup ${rustup_version} ${triple}" "$published" "${rustup_pins[$i]:-}"
  i=$((i + 1))
done

# Go: the index carries every file's sha256.
go_version=$(value go_version)
read -r -a go_pins <<<"$(pins_for go_sha256 | tr '\n' ' ')"
index=$(curl -fsSL 'https://go.dev/dl/?mode=json&include=all')
i=0
for arch in amd64 arm64; do
  published=$(printf '%s' "$index" | python3 -c '
import json, sys
version, arch = sys.argv[1], sys.argv[2]
for release in json.load(sys.stdin):
    if release["version"] == "go" + version:
        for file in release["files"]:
            if file["os"] == "linux" and file["arch"] == arch and file["kind"] == "archive":
                print(file["sha256"]); break
' "$go_version" "$arch")
  check "go ${go_version} linux-${arch}" "$published" "${go_pins[$i]:-}"
  i=$((i + 1))
done

# Node: the managed runtime the entrypoint links.
node_version=$(grep -oE 'version=[0-9]+\.[0-9]+\.[0-9]+; \\$' "$dockerfile" | grep -v '^version=2\.' | head -1 | cut -d= -f2 | tr -d '; \\')
if [ -n "$node_version" ]; then
  sums=$(curl -fsSL "https://nodejs.org/dist/v${node_version}/SHASUMS256.txt")
  for platform in linux-x64 linux-arm64; do
    pinned=$(grep -A1 "platform=${platform};" "$dockerfile" | grep -oE 'sha256=[0-9a-f]{64}' | head -1 | cut -d= -f2)
    published=$(printf '%s\n' "$sums" | grep " node-v${node_version}-${platform}.tar.gz$" | cut -c1-64)
    check "node ${node_version} ${platform}" "$published" "$pinned"
  done
fi

# gh: the release publishes a checksums file.
gh_version=$(grep -oE 'version=2\.[0-9]+\.[0-9]+; \\$' "$dockerfile" | head -1 | cut -d= -f2 | tr -d '; \\')
if [ -n "$gh_version" ]; then
  sums=$(curl -fsSL "https://github.com/cli/cli/releases/download/v${gh_version}/gh_${gh_version}_checksums.txt")
  for platform in linux_amd64 linux_arm64; do
    pinned=$(grep -A1 "platform=${platform};" "$dockerfile" | grep -oE 'sha256=[0-9a-f]{64}' | head -1 | cut -d= -f2)
    published=$(printf '%s\n' "$sums" | grep " gh_${gh_version}_${platform}.tar.gz$" | cut -c1-64)
    check "gh ${gh_version} ${platform}" "$published" "$pinned"
  done
fi

if [ "$failures" -ne 0 ]; then
  echo "$failures pin(s) disagree with their publisher" >&2
  exit 1
fi
