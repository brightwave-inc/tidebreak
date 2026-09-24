#!/bin/sh
# Prepare this directory for `docker compose up -d`: write the .env, tokens,
# and secret.key files a new Tidebreak self-host deployment needs.
#
#   ./setup.sh [--admin USER] [--domain NAME] [--version X.Y.Z] [--dir PATH]
#
# A value missing from the flags is asked for when the terminal is
# interactive. The script never overwrites a file that already exists: it
# keeps it and says so, so running it again is safe.
#
# See docs/self-hosting.md.
set -eu

# Nothing this script creates is readable by anyone else, even for a moment.
umask 077

# The server's image runs as this uid (deploy/self-host/Dockerfile).
server_uid=10001
repository=brightwave-inc/tidebreak

usage() {
	cat <<'EOF'
Usage: setup.sh [--admin USER] [--domain NAME] [--version X.Y.Z] [--dir PATH]

Writes .env, tokens, and secret.key for docker-compose.yml, then prints the
admin token and the next command. Existing files are kept, never overwritten.

  --admin USER      User id of the first administrator (1-64 characters from
                    A-Z a-z 0-9 . _ @ -).
  --domain NAME     Domain name that points at this machine. Caddy then serves
                    Tidebreak over HTTPS on it. Pass --domain "" to stay on
                    http://127.0.0.1:8080.
  --version X.Y.Z   Tidebreak release to run. Defaults to the latest release.
  --dir PATH        Where to write the files. Defaults to this script's
                    directory, beside docker-compose.yml.
EOF
}

die() {
	printf 'setup.sh: %s\n' "$*" >&2
	exit 1
}

interactive() {
	[ -t 0 ]
}

# ask QUESTION: print QUESTION and read one line into $answer.
ask() {
	printf '%s' "$1" >&2
	answer=
	IFS= read -r answer || true
}

value_of() {
	[ $# -ge 2 ] || die "$1 needs a value"
	printf '%s' "$2"
}

admin=
admin_given=
domain=
domain_given=
version=
dir=

while [ $# -gt 0 ]; do
	case $1 in
	--admin) admin=$(value_of "$@"); admin_given=1; shift 2 ;;
	--admin=*) admin=${1#*=}; admin_given=1; shift ;;
	--domain) domain=$(value_of "$@"); domain_given=1; shift 2 ;;
	--domain=*) domain=${1#*=}; domain_given=1; shift ;;
	--version) version=$(value_of "$@"); shift 2 ;;
	--version=*) version=${1#*=}; shift ;;
	--dir) dir=$(value_of "$@"); shift 2 ;;
	--dir=*) dir=${1#*=}; shift ;;
	-h | --help) usage; exit 0 ;;
	*) die "unknown option $1 (see --help)" ;;
	esac
done

if [ -z "$dir" ]; then
	dir=$(dirname -- "$0")
fi
[ -d "$dir" ] || die "no directory at $dir"
dir=$(cd -- "$dir" && pwd)

command -v openssl >/dev/null 2>&1 || die "openssl is required to generate the secrets"

env_file=$dir/.env
tokens_file=$dir/tokens
key_file=$dir/secret.key

exists() {
	[ -e "$1" ] || [ -L "$1" ]
}

# ---- gather and check every value before writing anything ----------------

if ! exists "$tokens_file"; then
	if [ -z "$admin_given" ] && interactive; then
		ask "User id for the first administrator [admin]: "
		admin=${answer:-admin}
	fi
	[ -n "$admin" ] || die "name the first administrator with --admin USER"
	case $admin in
	*[!A-Za-z0-9._@-]*) die "admin user id must use only A-Z a-z 0-9 . _ @ -" ;;
	esac
	[ "${#admin}" -le 64 ] || die "admin user id must be at most 64 characters"
fi

if ! exists "$env_file"; then
	if [ -z "$domain_given" ] && interactive; then
		ask "Domain name for HTTPS, pointed at this machine (empty to stay on 127.0.0.1): "
		domain=$answer
	fi
	case $domain in
	*[!A-Za-z0-9.-]* | .* | -* | *. | *-)
		die "domain must be a plain host name such as tidebreak.example.com, with no scheme, port, or path"
		;;
	esac

	if [ -z "$version" ]; then
		# The newest published release, from GitHub's API.
		if command -v curl >/dev/null 2>&1; then
			version=$(curl -fsSL --max-time 15 "https://api.github.com/repos/$repository/releases/latest" 2>/dev/null |
				sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1) || version=
		fi
		if [ -z "$version" ] && interactive; then
			ask "Tidebreak release to run (for example 0.116.0): "
			version=$answer
		fi
		[ -n "$version" ] || die "could not look up the latest release; pass --version X.Y.Z"
	fi
	version=${version#v}
	expr "$version" : '[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$' >/dev/null ||
		die "version must be a release number such as 0.116.0, got $version"
fi

# ---- write ----------------------------------------------------------------

# The host group the server joins to read tokens and secret.key.
host_gid=$(id -g)

# write_new PATH: copy stdin to PATH, which must not exist yet. `set -C`
# refuses to replace a file that appeared since the check above.
write_new() {
	(set -C && cat >"$1") || die "could not create $1"
}

# On Linux the server's uid cannot read a file owned by you with mode 0600,
# so these two get group read for your own group, which docker-compose.yml
# adds to the server. Docker Desktop and OrbStack on macOS share files with
# the container's uid already, so there they stay 0600.
share_with_server() {
	[ "$(uname -s)" = Darwin ] && return 0
	chgrp "$host_gid" "$1" && chmod 0640 "$1"
}

created=
kept=

if exists "$env_file"; then
	kept="$kept .env"
	domain=$(sed -n 's/^TIDEBREAK_DOMAIN=//p' "$env_file" | tail -n 1)
else
	{
		printf '# Written by setup.sh. Keep a private backup of this file.\n'
		printf '# The Tidebreak release docker compose pulls.\n'
		printf 'TIDEBREAK_VERSION=%s\n' "$version"
		printf '# Only PostgreSQL and the server use this password.\n'
		printf 'POSTGRES_PASSWORD=%s\n' "$(openssl rand -hex 32)"
		printf '# The group that owns tokens and secret.key; the server joins it to read them.\n'
		printf 'TIDEBREAK_HOST_GID=%s\n' "$host_gid"
		if [ -n "$domain" ]; then
			printf '# HTTPS through the caddy service, on this domain.\n'
			printf 'TIDEBREAK_DOMAIN=%s\n' "$domain"
			printf 'TIDEBREAK_PUBLIC_URL=https://%s\n' "$domain"
			printf 'COMPOSE_PROFILES=tls\n'
		fi
	} | write_new "$env_file"
	created="$created .env"
fi

token=
if exists "$tokens_file"; then
	kept="$kept tokens"
else
	token=$(openssl rand -hex 32)
	{
		printf '# user-id  token  [admin|service]. See docs/self-hosting.md.\n'
		printf '%s %s admin\n' "$admin" "$token"
	} | write_new "$tokens_file"
	share_with_server "$tokens_file"
	created="$created tokens"
fi

if exists "$key_file"; then
	kept="$kept secret.key"
else
	openssl rand -base64 32 | write_new "$key_file"
	share_with_server "$key_file"
	created="$created secret.key"
fi

# ---- report ---------------------------------------------------------------

[ -z "$created" ] || printf 'Created:%s\n' "$created"
[ -z "$kept" ] || printf 'Kept, unchanged:%s\n' "$kept"

# A kept file keeps its mode. On Linux, say when the server could not read it.
if [ "$(uname -s)" != Darwin ]; then
	for file in "$tokens_file" "$key_file"; do
		case " $kept " in *" ${file##*/} "*) ;; *) continue ;; esac
		# ls -ln is the portable way to read a mode and a numeric owner.
		# shellcheck disable=SC2012,SC2046
		set -- $(ls -ln "$file")
		case $1 in
		-???r*) ;;
		*)
			[ "$3" = "$server_uid" ] ||
				printf 'Warning: the server cannot read %s. Run: chgrp %s %s && chmod 0640 %s\n' \
					"${file##*/}" "$host_gid" "$file" "$file" >&2
			;;
		esac
	done
fi

if [ -n "$token" ] && [ "$(uname -s)" != Darwin ] && [ "$(id -gn)" != "$(id -un)" ]; then
	printf 'Warning: your group %s may include other users, who can read tokens and secret.key.\n' "$(id -gn)" >&2
	printf 'To keep them to the server alone, run: sudo chown %s tokens secret.key && sudo chmod 0600 tokens secret.key\n' "$server_uid" >&2
fi

if [ -n "$domain" ]; then
	address=https://$domain
else
	address=http://127.0.0.1:8080
fi

if [ -n "$token" ]; then
	printf '\nAdmin user: %s\n' "$admin"
	printf 'Admin token, shown once (it is also in tokens):\n\n  %s\n' "$token"
fi

printf '\nNext:\n\n'
[ "$(pwd)" = "$dir" ] || printf '  cd %s\n' "$dir"
printf '  docker compose up -d\n\n'
printf 'Then open %s and sign in with the admin token.\n' "$address"
