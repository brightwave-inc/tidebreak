#!/bin/sh
# Prepare this directory for `docker compose up -d`: write the .env, tokens,
# and secret.key files a new Tidebreak self-host deployment needs.
#
#   ./setup.sh [--admin USER] [--domain NAME] [--version X.Y.Z | --build] [--dir PATH]
#
# A value missing from the flags is asked for when the terminal is
# interactive. The script never overwrites a file that already exists: it
# keeps it and says so, so running it again is safe. A kept .env only gains
# the settings docker-compose.yml needs and it lacks, and the script names
# each one it adds.
#
# See docs/self-hosting.md.
set -eu

# Nothing this script creates is readable by anyone else, even for a moment.
umask 077

# The server's image runs as this uid (deploy/self-host/Dockerfile).
server_uid=10001
repository=brightwave-inc/tidebreak
# The first release that can keep blobs on local disk, which
# docker-compose.yml does by default. An older server refuses to start with
# that setting.
min_version=0.117.0
build_files=docker-compose.yml:docker-compose.build.yml

usage() {
	cat <<EOF
Usage: setup.sh [--admin USER] [--domain NAME] [--version X.Y.Z | --build] [--dir PATH]

Writes .env, tokens, and secret.key for docker-compose.yml, then prints the
admin token and the next command. Existing files are kept, never overwritten.

  --admin USER      User id of the first administrator (1-64 characters from
                    A-Z a-z 0-9 . _ @ -).
  --domain NAME     Domain name that points at this machine. Caddy then serves
                    Tidebreak over HTTPS on it. Pass --domain "" to stay on
                    http://127.0.0.1:8080.
  --version X.Y.Z   Tidebreak release to pull, $min_version or later. Defaults
                    to the latest release.
  --build           Build the server image from this checkout instead of
                    pulling a release. The default while no release is
                    $min_version or later.
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

# shell_quote WORD: WORD as the shell would need it typed.
shell_quote() {
	case $1 in
	'' | *[!A-Za-z0-9_./:@%+=-]*)
		printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
		;;
	*) printf '%s' "$1" ;;
	esac
}

# valid_domain NAME: a host name of dot-separated labels, each 1-63
# characters of letters, digits, and inner hyphens.
valid_domain() {
	case $1 in
	'' | *[!A-Za-z0-9.-]* | .* | *. | *..*) return 1 ;;
	esac
	[ "${#1}" -le 253 ] || return 1
	set -f
	old_ifs=$IFS
	IFS=.
	# shellcheck disable=SC2086 # Split on the dots.
	set -- $1
	IFS=$old_ifs
	set +f
	for label in "$@"; do
		case $label in
		-* | *-) return 1 ;;
		esac
		[ "${#label}" -le 63 ] || return 1
	done
}

# release_number VALUE: VALUE without a leading v, if it is X.Y.Z.
release_number() {
	number=${1#v}
	expr "$number" : '[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$' >/dev/null || return 1
	printf '%s' "$number"
}

# at_least A B: whether release A is B or later.
at_least() {
	[ "$(printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n | head -n 1)" = "$2" ]
}

# env_value KEY: KEY's last value in .env, without surrounding quotes.
env_value() {
	value=$(sed -n "s/^[[:space:]]*$1[[:space:]]*=//p" "$env_file" | tail -n 1)
	value=${value#\"}
	value=${value%\"}
	value=${value#\'}
	value=${value%\'}
	printf '%s' "$value"
}

admin=
admin_given=
domain=
domain_given=
version=
build=
dir=

while [ $# -gt 0 ]; do
	case $1 in
	--admin) admin=$(value_of "$@"); admin_given=1; shift 2 ;;
	--admin=*) admin=${1#*=}; admin_given=1; shift ;;
	--domain) domain=$(value_of "$@"); domain_given=1; shift 2 ;;
	--domain=*) domain=${1#*=}; domain_given=1; shift ;;
	--version) version=$(value_of "$@"); shift 2 ;;
	--version=*) version=${1#*=}; shift ;;
	--build) build=1; shift ;;
	--dir) dir=$(value_of "$@"); shift 2 ;;
	--dir=*) dir=${1#*=}; shift ;;
	-h | --help) usage; exit 0 ;;
	*) die "unknown option $1 (see --help)" ;;
	esac
done

[ -z "$build" ] || [ -z "$version" ] || die "choose --version or --build, not both"

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

# choose_image: set $version, and $build when this stack builds from source.
choose_image() {
	if [ -n "$build" ]; then
		return 0
	fi
	if [ -n "$version" ]; then
		number=$(release_number "$version") ||
			die "version must be a release number such as $min_version, got $version"
		at_least "$number" "$min_version" ||
			die "Tidebreak $number cannot keep blobs on local disk, which this stack does by default. Pass --version $min_version or later, or --build to build this checkout."
		version=$number
		return 0
	fi
	latest=
	if command -v curl >/dev/null 2>&1; then
		latest=$(curl -fsSL --max-time 15 "https://api.github.com/repos/$repository/releases/latest" 2>/dev/null |
			sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1) || latest=
	fi
	if latest=$(release_number "$latest"); then
		if at_least "$latest" "$min_version"; then
			version=$latest
		else
			build=1
			printf 'The latest release, %s, cannot keep blobs on local disk; %s is the first that can.\n' "$latest" "$min_version"
			printf 'Until it is published, docker compose builds the server from this checkout.\n\n'
		fi
		return 0
	fi
	if interactive; then
		ask "Could not look up the latest release. Release to run ($min_version or later), or empty to build this checkout: "
		if [ -n "$answer" ]; then
			version=$answer
		else
			build=1
		fi
		choose_image
		return 0
	fi
	die "could not look up the latest release; pass --version X.Y.Z ($min_version or later), or --build"
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

system=$(uname -s)
as_root=
[ "$(id -u)" != 0 ] || as_root=1

# The host group the server joins to read tokens and secret.key: your own,
# or the server's own when root hands the files to the server's uid. A kept
# .env already names one, and files created now must belong to it.
host_gid=$(id -g)
if [ -n "$as_root" ] && [ "$system" != Darwin ]; then
	host_gid=$server_uid
fi
env_additions=
if exists "$env_file"; then
	kept_gid=$(env_value TIDEBREAK_HOST_GID)
	if [ -n "$kept_gid" ]; then
		host_gid=$kept_gid
	else
		env_additions=TIDEBREAK_HOST_GID
	fi
	if [ -z "$(env_value TIDEBREAK_VERSION)" ]; then
		choose_image
		env_additions="$env_additions TIDEBREAK_VERSION"
	fi
	domain=$(env_value TIDEBREAK_DOMAIN)
else
	if [ -z "$domain_given" ] && interactive; then
		ask "Domain name for HTTPS, pointed at this machine (empty to stay on 127.0.0.1): "
		domain=$answer
	fi
	if [ -n "$domain" ] && ! valid_domain "$domain"; then
		die "domain must be a plain host name such as tidebreak.example.com, with no scheme, port, or path"
	fi
	choose_image
fi

if [ "$system" != Darwin ] && [ -z "$as_root" ]; then
	if ! exists "$tokens_file" || ! exists "$key_file"; then
		case " $(id -G) " in
		*" $host_gid "*) ;;
		*) die ".env sets TIDEBREAK_HOST_GID=$host_gid, which is not one of your groups. Run setup.sh as a member of that group, or change TIDEBREAK_HOST_GID in .env." ;;
		esac
	fi
fi

# ---- write ----------------------------------------------------------------

# write_new PATH: copy stdin to PATH, which must not exist yet. `set -C`
# refuses to replace a file that appeared since the check above.
write_new() {
	(set -C && cat >"$1") || die "could not create $1"
}

# On Linux the server's uid cannot read a file owned by you with mode 0600,
# so these two get group read for the group docker-compose.yml adds to the
# server. Run as root, the script hands them to the server's uid instead.
# Docker Desktop and OrbStack on macOS share files with the container's uid
# already, so there they stay 0600.
share_with_server() {
	if [ "$system" = Darwin ]; then
		return 0
	fi
	if [ -n "$as_root" ]; then
		chown "$server_uid:$server_uid" "$1" || die "could not give $1 to uid $server_uid"
		return 0
	fi
	if ! chgrp "$host_gid" "$1" || ! chmod 0640 "$1"; then
		die "could not give $1 to group $host_gid"
	fi
}

created=
kept=

if exists "$env_file"; then
	[ -n "$env_additions" ] || kept="$kept .env"
	if [ -n "$env_additions" ]; then
		# Keep every existing line, and start the additions on a line of their own.
		[ -z "$(tail -c 1 "$env_file")" ] || printf '\n' >>"$env_file"
		{
			printf '# Added by setup.sh.\n'
			case " $env_additions " in
			*" TIDEBREAK_HOST_GID "*) printf 'TIDEBREAK_HOST_GID=%s\n' "$host_gid" ;;
			esac
			case " $env_additions " in
			*" TIDEBREAK_VERSION "*)
				if [ -n "$build" ]; then
					printf 'COMPOSE_FILE=%s\n' "$build_files"
					printf 'TIDEBREAK_VERSION=%s\n' "$min_version"
				else
					printf 'TIDEBREAK_VERSION=%s\n' "$version"
				fi
				;;
			esac
		} >>"$env_file"
	fi
else
	{
		printf '# Written by setup.sh. Keep a private backup of this file.\n'
		if [ -n "$build" ]; then
			printf '# docker compose builds the server image from this checkout. To pull a\n'
			printf '# published release instead, delete COMPOSE_FILE and set TIDEBREAK_VERSION\n'
			printf '# to %s or later.\n' "$min_version"
			printf 'COMPOSE_FILE=%s\n' "$build_files"
			printf 'TIDEBREAK_VERSION=%s\n' "$min_version"
		else
			printf '# The Tidebreak release docker compose pulls.\n'
			printf 'TIDEBREAK_VERSION=%s\n' "$version"
		fi
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
[ -z "$env_additions" ] || printf 'Added to the kept .env: %s\n' "$(printf '%s' "$env_additions" | sed 's/^ *//')"

if [ -z "$(env_value POSTGRES_PASSWORD)" ]; then
	printf 'Warning: .env sets no POSTGRES_PASSWORD. Add the password your database was created with.\n' >&2
fi
kept_version=$(env_value TIDEBREAK_VERSION)
if [ -z "$(env_value COMPOSE_FILE)" ] && [ -z "$(env_value TIDEBREAK_BLOB_STORE_URL)" ] &&
	number=$(release_number "$kept_version") && ! at_least "$number" "$min_version"; then
	printf 'Warning: .env runs Tidebreak %s, which cannot keep blobs on local disk. Set TIDEBREAK_VERSION to %s or later, or TIDEBREAK_BLOB_STORE_URL to your bucket.\n' \
		"$number" "$min_version" >&2
fi

# A kept file keeps its owner and mode. On Linux, say when the server could
# not read it.
if [ "$system" != Darwin ]; then
	for file in "$tokens_file" "$key_file"; do
		case " $kept " in *" ${file##*/} "*) ;; *) continue ;; esac
		# ls -ln is the portable way to read a mode and numeric owners.
		# shellcheck disable=SC2012,SC2046
		set -- $(ls -ln "$file")
		readable=
		case $1 in -r*) [ "$3" != "$server_uid" ] || readable=1 ;; esac
		case $1 in -???r*) [ "$4" != "$host_gid" ] || readable=1 ;; esac
		case $1 in -??????r*) readable=1 ;; esac
		[ -n "$readable" ] ||
			printf 'Warning: the server cannot read %s. Run: chgrp %s %s && chmod 0640 %s\n' \
				"${file##*/}" "$host_gid" "$(shell_quote "$file")" "$(shell_quote "$file")" >&2
	done
fi

if [ -n "$token" ] && [ "$system" != Darwin ] && [ -z "$as_root" ] && [ "$(id -gn)" != "$(id -un)" ]; then
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
[ "$(pwd)" = "$dir" ] || printf '  cd %s\n' "$(shell_quote "$dir")"
printf '  docker compose up -d\n\n'
if [ -n "$(env_value COMPOSE_FILE)" ]; then
	printf 'The first up builds the server image from this checkout. That takes a while\n'
	printf 'and several GB of memory.\n'
fi
printf 'Then open %s and sign in with the admin token.\n' "$address"
