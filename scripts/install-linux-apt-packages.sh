#!/usr/bin/env bash
# Use HTTPS Ubuntu archives for Linux CI and release dependencies.
#
# Hosted runner mirrors can stall on HTTP requests. Normalize Ubuntu sources
# to official HTTPS endpoints, then install with short timeouts and retries.
set -euo pipefail

if (($# == 0)); then
  echo "usage: scripts/install-linux-apt-packages.sh PACKAGE [PACKAGE...]" >&2
  exit 2
fi

export DEBIAN_FRONTEND=noninteractive

root="${TIDEBREAK_APT_ROOT:-}"
if [[ -n "$root" ]]; then
  sources_list="$root/etc/apt/sources.list"
  sources_dir="$root/etc/apt/sources.list.d"
  mirrors_file="$root/etc/apt/apt-mirrors.txt"
  apt_conf_dir="$root/etc/apt/apt.conf.d"
else
  sources_list=/etc/apt/sources.list
  sources_dir=/etc/apt/sources.list.d
  mirrors_file=/etc/apt/apt-mirrors.txt
  apt_conf_dir=/etc/apt/apt.conf.d
fi

if [[ -z "${TIDEBREAK_APT_DRY_RUN:-}" && "$(id -u)" -ne 0 ]]; then
  sudo_prefix=(sudo)
else
  sudo_prefix=()
fi

run() {
  if ((${#sudo_prefix[@]})); then
    "${sudo_prefix[@]}" "$@"
  else
    "$@"
  fi
}

if [[ -n "${TIDEBREAK_APT_CODENAME:-}" ]]; then
  codename="$TIDEBREAK_APT_CODENAME"
elif [[ -r /etc/os-release ]]; then
  # shellcheck disable=SC1091
  codename="$(. /etc/os-release && printf '%s\n' "$VERSION_CODENAME")"
else
  echo "unable to determine Ubuntu codename" >&2
  exit 1
fi

if [[ -n "${TIDEBREAK_APT_ARCH:-}" ]]; then
  arch="$TIDEBREAK_APT_ARCH"
elif command -v dpkg >/dev/null; then
  arch="$(dpkg --print-architecture)"
else
  echo "unable to determine dpkg architecture" >&2
  exit 1
fi

case "$arch" in
amd64)
  archive_url="${TIDEBREAK_APT_ARCHIVE_URL:-https://archive.ubuntu.com/ubuntu}"
  security_url="${TIDEBREAK_APT_SECURITY_URL:-https://security.ubuntu.com/ubuntu}"
  ;;
arm64)
  archive_url="${TIDEBREAK_APT_ARCHIVE_URL:-https://ports.ubuntu.com/ubuntu-ports}"
  security_url="${TIDEBREAK_APT_SECURITY_URL:-$archive_url}"
  ;;
*)
  echo "unsupported apt architecture: $arch" >&2
  exit 1
  ;;
esac

rewrite_ubuntu_sources() {
  local file="$1"
  [[ -f "$file" ]] || return 0
  run python3 -c '
from pathlib import Path
import re
import sys

path = Path(sys.argv[1])
archive_url = sys.argv[2]
security_url = sys.argv[3]
text = path.read_text()
ubuntu_source = re.compile(
    r"https?://(?:(?P<security>security\.ubuntu\.com/ubuntu)"
    r"|(?:azure\.)?archive\.ubuntu\.com/ubuntu(?:-ports)?"
    r"|ports\.ubuntu\.com/ubuntu-ports)(?=/|\s|$)"
)
# One pass preserves explicit overrides that name another Ubuntu endpoint.
text = ubuntu_source.sub(
    lambda match: security_url if match.group("security") else archive_url,
    text,
)
path.write_text(text)
' "$file" "$archive_url" "$security_url"
}

run mkdir -p "$(dirname "$sources_list")" "$sources_dir" "$apt_conf_dir"

if [[ -f "$sources_list" ]]; then
  rewrite_ubuntu_sources "$sources_list"
else
  cat <<SOURCES | run tee "$sources_list" >/dev/null
deb $archive_url $codename main restricted universe multiverse
deb $archive_url $codename-updates main restricted universe multiverse
deb $archive_url $codename-backports main restricted universe multiverse
deb $security_url $codename-security main restricted universe multiverse
SOURCES
fi

shopt -s nullglob
for file in "$sources_dir"/*.list "$sources_dir"/*.sources; do
  rewrite_ubuntu_sources "$file"
done
shopt -u nullglob

if [[ -e "$mirrors_file" || -n "$root" ]]; then
  printf '%s\tpriority:1\n' "$archive_url" | run tee "$mirrors_file" >/dev/null
fi

cat <<CONF | run tee "$apt_conf_dir/99tidebreak-ci" >/dev/null
Acquire::Retries "3";
Acquire::http::Timeout "20";
Acquire::https::Timeout "20";
Acquire::ftp::Timeout "20";
CONF

apt_opts=(
  -o Acquire::Retries=3
  -o Acquire::http::Timeout=20
  -o Acquire::https::Timeout=20
  -o Dpkg::Use-Pty=0
)

if [[ -n "${TIDEBREAK_APT_DRY_RUN:-}" ]]; then
  printf 'would run: apt-get %s update\n' "${apt_opts[*]}"
  printf 'would run: apt-get %s install -y --no-install-recommends' "${apt_opts[*]}"
  printf ' %s' "$@"
  printf '\n'
  exit 0
fi

run apt-get "${apt_opts[@]}" update
run apt-get "${apt_opts[@]}" install -y --no-install-recommends "$@"
