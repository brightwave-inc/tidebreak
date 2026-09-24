# Self-hosting Tidebreak

The Tidebreak desktop app is the primary product: it runs the whole engine
locally, keeps state in SQLite under your own home directory, and needs no
server. **Self-hosting is for something else** — a team that wants one shared
deployment inside its own network (a VM, a VPC, an office server), with named
users, a shared PostgreSQL database, and shared provider credentials the
operator manages.

This guide covers running that deployment from the packaging in
`deploy/self-host/`. One script prepares it, and the stack runs on a single
machine with no other service: PostgreSQL, document storage, the encrypted
credential store, and HTTPS all run in one Compose project. The only outside
traffic goes to your model provider, the image registries, and, with a
domain, the certificate authority that issues its certificate. This guide
describes only behavior verified in the code on this branch; where something
is not built yet, it says so.

## Prerequisites

- A Linux machine with Docker Engine and Docker Compose v2, and a user that
  can run `docker`. `docker compose version` prints the Compose version.
  Docker Desktop or OrbStack on a Mac works for trying it out.
- `openssl`, which `setup.sh` uses to generate the secrets.
- A model provider the machine can reach, such as an Anthropic or OpenAI API
  key.
- Optional: a domain name whose DNS record points at the machine, with ports
  80 and 443 open to the internet. With a domain, Caddy serves Tidebreak over
  HTTPS and renews its certificate. Without one, Tidebreak answers only on
  `127.0.0.1:8080` of the machine.

## Quickstart

To start a deployment, do the following on the machine:

1. Get the deployment files:

   ```sh
   git clone https://github.com/brightwave-inc/tidebreak.git
   cd tidebreak/deploy/self-host
   ```

2. Write the configuration. Name the first administrator, and pass your
   domain if you have one:

   ```sh
   ./setup.sh --admin alice --domain tidebreak.example.com
   ```

   Without flags, the script asks for each value. It writes `.env`, `tokens`,
   and `secret.key` beside `docker-compose.yml` and prints the admin token
   once. Keep the token somewhere private; it is also the admin line in
   `tokens`.

3. Start the stack:

   ```sh
   docker compose up -d
   ```

   Compose pulls the release that `setup.sh` recorded in `.env` and starts
   PostgreSQL, the server, and, with a domain, Caddy. The stack keeps blobs
   on local disk, which needs release 0.117.0 or later. Until such a release
   is published, `setup.sh` sets the stack to build the server image from
   this checkout instead, and says so. The first `up` then compiles, which
   takes a while and several GB of memory.

4. Open `https://tidebreak.example.com`, or `http://127.0.0.1:8080` without a
   domain, and paste the admin token to sign in.

To check the server from the machine itself, read `/healthz`. The answer names
the release and the API level it runs:

```sh
curl -fsS http://127.0.0.1:8080/healthz
# -> {"status":"ok","version":"0.117.0","api_level":1}
```

`/healthz` and `/version` answer without a token, and so do the sign-in routes
under `/auth/`. Everything else needs `Authorization: Bearer <token>` with a
token from your file. `/version` answers `{"version", "api_level"}`, and every
client reads it before it attaches: a client too old or too new for the
machine says which side to update instead of failing later.

Without a domain, the address answers only on the machine. To reach it from
your own computer, forward the port over SSH, and then open
`http://127.0.0.1:8080` in your browser:

```sh
ssh -L 8080:127.0.0.1:8080 <you>@<your server>
```

To give the deployment a model, sign in as an administrator and save a
provider key in **Settings**. Tidebreak stores it encrypted in PostgreSQL. A
provider variable such as `ANTHROPIC_API_KEY` in `.env` also works, as a
fallback.

### What setup.sh writes

| File | Mode | What it holds |
| --- | --- | --- |
| `.env` | `0600` | `TIDEBREAK_VERSION`, the release Compose runs; a random `POSTGRES_PASSWORD`; `TIDEBREAK_HOST_GID`; with a domain, `TIDEBREAK_DOMAIN`, `TIDEBREAK_PUBLIC_URL`, and `COMPOSE_PROFILES=tls`; and, when the stack builds from source, `COMPOSE_FILE`. |
| `tokens` | `0640` | One admin line: the user id you named and a random 64-character token. |
| `secret.key` | `0640` | 32 random bytes in base64: the key that encrypts stored credentials. |

`--version X.Y.Z` picks the release, which must be 0.117.0 or later; an
older server cannot keep blobs on local disk and refuses to start. Without
`--version`, the script asks GitHub for the latest release. When that release
is older than 0.117.0, or when you pass `--build`, the script writes
`COMPOSE_FILE=docker-compose.yml:docker-compose.build.yml` to `.env`, so every
Compose command builds the server image from this checkout. To pull a
published release later, delete that line and set `TIDEBREAK_VERSION` to
0.117.0 or later.

The script never overwrites a file. When you run it again, it keeps every file
that exists, says which ones it kept, and writes only the missing ones. A kept
`.env` that lacks `TIDEBREAK_HOST_GID` or `TIDEBREAK_VERSION`, such as one
written for an earlier version of this stack, gets the missing lines added at
the end, and the script names each one. Git ignores all three files.

The server runs as uid 10001 inside its container, so on a Linux host it
cannot read a file that only your user can read. That is why `setup.sh` makes
`tokens` and `secret.key` readable by your own group and records that group as
`TIDEBREAK_HOST_GID`. `docker-compose.yml` adds the group to the server, which
can then read both files, while other users of the host still cannot. This
assumes that your primary group holds only you, which is the default on
Debian, Ubuntu, and Fedora, where each user gets a group of the same name. The
script warns when your group has a different name. Run as root, the script
gives both files to uid 10001 with mode `0600` instead. On macOS, Docker
Desktop and OrbStack share files with the container's user already, so there
both files stay `0600`. Rootless Docker and rootless Podman map uids and
groups differently, so group sharing does not reach the server there;
[Secrets in the database](#secrets-in-the-database) shows how to hand both
files to the container's user instead.

Keep the group read on both files. `chmod g-r tokens` or `chmod g-r
secret.key` removes the server's only access to that file, and the server then
stops at boot.

If you create or replace these files by hand on Linux, keep the same shape:

```sh
chgrp "$(id -g)" tokens secret.key
chmod 0640 tokens secret.key
```

A server that cannot read `tokens` stops at boot with `failed to read auth
tokens file /run/tidebreak/tokens: Permission denied`.

### Build the image from source

Unless `.env` sets `COMPOSE_FILE`, `docker compose up -d` only pulls the
published image. To build the server image from this checkout instead, add the
build file to the command:

```sh
docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --build
```

A cold build compiles the Rust workspace and the desktop renderer, which takes
a while and several GB of memory. The image is tagged
`tidebreak-self-host:local` and reports its version as `0.0.0-unreleased`.
Pass the same two `-f` flags to every later Compose command, or the next `up`
switches back to the published image. To make the build file the default for
every command, add
`COMPOSE_FILE=docker-compose.yml:docker-compose.build.yml` to `.env`, as
`setup.sh --build` does.

## Identity

Every request to a self-host server must name a user. Choose one of these ways
to name them:

- A token file, which the quickstart sets up. You list each user and token in
  `tokens`.
- An OpenID Connect provider, so people sign in with the accounts your
  organization already has. The token file stays beside it for the first
  administrator and for command-line access.
- Model Gateway, when your organization runs one. The machine then accepts the
  short-lived tokens it issues for the accounts it manages.

The server refuses to start without one of these, and it refuses to combine
Model Gateway with either of the others.

### Token file

The token file is the credential-to-principal map, and it is also where roles
are managed — there is deliberately no UI for that. One line per token,
whitespace-separated, `#` comments and blank lines ignored:

```text
# user-id  token                   role
alice      <64 hex characters>     admin
bob        <64 hex characters>
```

Generate each token with:

```sh
openssl rand -hex 32
```

Rules the loader enforces:

- User ids are 1 to 64 characters from `[A-Za-z0-9._@-]`.
- Tokens are at least **32 characters** drawn from `[A-Za-z0-9._~-]`. Thirty-two
  random bytes in hex gives 64 characters, comfortably over the floor.
- The optional third field is `admin` or `service`. `admin` puts a person on
  the deployment plane. `service` names a member that owns automated sessions
  and never signs in. An absent field means a person member. Combining
  `admin` and `service`, or any other value, is a parse error rather than a
  silent demotion.
- **At least one person line must say `admin`**, or the file fails to load and
  the server does not start. A service line does not satisfy that check. A
  deployment nobody is empowered to configure must not exist.
- A user's lines must agree about their role. A file that says both fails to
  load.
- One user may hold several tokens, which is how rotation works. A token may
  name only one user, and a duplicate token fails the load.

The server reads the file once, at boot. To add a member, append a line and
restart the server:

```sh
printf 'bob %s\n' "$(openssl rand -hex 32)" >> tokens
docker compose restart server
```

To revoke a token, delete its line and restart the server the same way.

Give `admin` only to the people who actually administer the deployment: MCP
server definitions spawn processes on the host, and the provider credentials
are shared.

#### What a member can and cannot do

Members get their own chats, projects, documents, transcripts, and event
stream, plus the read-only discovery a client needs in order to work — the
model list, the plugin catalog, the app library. They get `403` on the
**deployment plane**: MCP server configuration, provider and web-search and
code-execution credentials (including the presence reads that reveal secret
metadata), model role assignments, settings writes, plugin install and enable,
and connected-app sign-in and sign-out.

That split is a property of the router rather than of individual handlers, so
a configuration route cannot quietly land outside the gate. The reasoning, the
rejected alternatives, and what would make us revisit it are in
[decision record 6](decisions/0006-self-host-deployment-plane-authorization.md).

#### What a member runs

A member with a token uses the browser app at the machine's address, the HTTP
API, or the `tidebreak` CLI pointed at the deployment. For the CLI, give each
teammate a token from the file and the base URL:

```sh
export TIDEBREAK_SERVER_URL=https://tidebreak.example
export TIDEBREAK_SERVER_TOKEN=<the member token>
cargo run -p tidebreak-cli -- --server "$TIDEBREAK_SERVER_URL" chat list
cargo run -p tidebreak-cli -- --server "$TIDEBREAK_SERVER_URL" -p "summarize yesterday"
```

`--server` / `TIDEBREAK_SERVER_TOKEN` are the same attach path the headless
docs describe. A member token receives `403` on deployment-plane routes, which
is the intended degradation — not a desktop Settings panel. Remote server URLs
must use HTTPS; cleartext HTTP is available only for loopback development,
such as the SSH forward in the quickstart.

The packaged desktop app still embeds its local Desktop-profile server, but it
can attach its renderer to a remote self-host machine. For a token-file
machine, use “Connect with token”, under Advanced in Settings → Model Gateway.
For a Gateway-backed machine, “Connect with Model Gateway” in the same panel
reuses the app's managed Gateway session and stores no Tidebreak user token.

### OpenID Connect

To let people sign in with your organization's identity provider, register a
client there with `https://<your domain>/auth/oidc/callback` as its redirect
URI. Then add the client to `.env` and run `docker compose up -d`:

```sh
TIDEBREAK_AUTH_OIDC_ISSUER=https://<your identity provider>
TIDEBREAK_AUTH_OIDC_CLIENT_ID=<client id>
TIDEBREAK_AUTH_OIDC_CLIENT_SECRET=<client secret>
```

OIDC needs `TIDEBREAK_PUBLIC_URL`, which `setup.sh --domain` writes, because
the callback returns there. The machine checks every sign-in itself; see
[Opening the machine in a browser](#opening-the-machine-in-a-browser) for the
flow and for `TIDEBREAK_AUTH_OIDC_CLAIM`, which picks the claim that becomes
the user id.

OIDC never makes anyone an administrator. Keep the token file: it names the
first administrator, and it is what CLI and script access uses.

### Model Gateway identity

When your organization runs Model Gateway, the machine can take its users from
it instead of a token file. Set `TIDEBREAK_AUTH_GATEWAY_URL` to the Model
Gateway base URL. Tidebreak desktop's “Connect with Model Gateway” flow
discovers this URL from the hosted machine, mints a short-lived `tidebreak`
resource token from the OAuth session the app already holds, and refreshes it
automatically.

The server asks the Gateway to resolve that token on every request. Active
Gateway users become Tidebreak members, Gateway administrators become
Tidebreak administrators, and the stable Gateway user UUID becomes the owner
key. Deactivation, session revocation, and role changes require no Tidebreak
roster update or token redistribution. If the Gateway cannot validate a token,
the request is refused.

Do not set `TIDEBREAK_AUTH_TOKENS_FILE` in this mode. Selecting both mechanisms
is an ambiguous configuration and the server refuses to start. The stock
`docker-compose.yml` sets it, so remove that line and the `tokens` mount from
the `server` service before you switch.

`TIDEBREAK_AUTH_GATEWAY_URL` must remain the public Gateway identity URL that
the desktop is signed into. If the hosted server cannot reach that URL from its
cluster, set `TIDEBREAK_AUTH_GATEWAY_VERIFIER_URL` to a cluster-routable HTTPS
URL. Only the server's principal checks use the override; `/auth/discovery`
continues to publish the public URL.

In this mode model access needs no configured credential. The server exchanges
each caller's Gateway token for a short-lived, inference-only token for the
same user and drives that caller's turns with it, so the Gateway meters every
turn to the person who ran it and the deployment holds no inference secret to
rotate. The exchanged tokens stay in the server's memory: they are never
stored, never logged, and never sent to a client. A caller whose Gateway
session is revoked loses model access on their next turn, which fails with the
same sign-in prompt the app already shows; other callers keep working.

A stored provider configuration still wins, and so do the provider environment
variables. Configure a provider in Settings, or set a credential such as
`ANTHROPIC_API_KEY` or an endpoint such as `ANTHROPIC_BASE_URL`, and that path
serves every caller exactly as it does today. Per-caller inference is the
default only for a provider the deployment states no other path to. It also
requires Gateway authentication: a server running on static tokens has no
caller token to exchange and keeps its environment-configured providers.

An attached client shows the Gateway under Settings → Model Gateway, read-only:
the machine names the Gateway it authenticates you against, and there is
nothing to sign in to or sign out of there. You already signed in on your own
computer, and the machine runs your work with that account. It never asks a
client to sign in to it, because it holds no Gateway session of its own and
could never report one.

## Secrets

Administrators save provider, web-search, code-execution, and connected-app
credentials in Settings. The stock stack keeps them in PostgreSQL, encrypted
with the key in `secret.key`. Credentials you pass as environment variables,
such as `ANTHROPIC_API_KEY` in `.env`, stay fallbacks: the server reads them
when nothing is stored for that provider.

### Secrets in the database

The server encrypts each stored secret with AES-256-GCM and keeps it in the
`deployment_secrets` table of its own PostgreSQL database
([decision record 102](decisions/0102-self-host-secrets-in-the-database.md)).
A dump or backup of the database alone reveals no secret. Anyone who holds
both the key file and the database can read every secret, and if you lose the
key file, you lose the secrets.

`setup.sh` creates the key once, beside `docker-compose.yml`: 32 random bytes
from `openssl rand -base64 32`, as one line of base64. `docker-compose.yml`
mounts it read-only at `/run/tidebreak/secret.key` and points
`TIDEBREAK_SECRET_KEY_FILE` at it. On a Linux host, `setup.sh` makes the file
readable by your own group, which the compose file adds to the server's uid,
as [What setup.sh writes](#what-setupsh-writes) describes. Outside this stack,
create the key the same way, make it readable by the uid that runs the
server and by nobody else, and set `TIDEBREAK_SECRET_KEY_FILE` to its path.

Rootless Docker and rootless Podman map uid 10001 inside the container to a
different uid on the host, so neither group sharing nor a plain `chown 10001`
reaches the right account. Let the runtime apply its own mapping instead. With
Podman, run `podman unshare chown 10001 secret.key`. With rootless Docker, run
`chown` in a throwaway container of the server image:

```sh
docker run --rm --user 0 --entrypoint chown \
  -v "$PWD/secret.key:/secret.key" ghcr.io/brightwave-inc/tidebreak-server:<version> 10001 /secret.key
```

Do the same for `tokens`.

Back up the key file separately from the database. A database backup without
the key restores no secret, and you would have to enter each one again. Keep
the two backups in different places, so one stolen backup never holds both.

The server reads the key once at boot. It refuses to start when the file is
missing, unreadable, or does not decode to exactly 32 bytes, when accounts
other than its owner can change it, and when the `TIDEBREAK_VAULT_*`
variables are set as well. It starts, with a warning, when the file's group or
every account on the machine can read it. The check follows symlinks, so a
Kubernetes secret mount is judged by the file it names.

With the files `setup.sh` writes, the group warning appears at every boot and
names your group's id. It is expected. The group is the setup account's own
primary group, which on most Linux distributions holds only that account, and
group read is how the server's uid reads the file. Do not follow the warning's
`chmod g-r` advice for `secret.key`, or for `tokens`, which the server reads
the same way: the server can then read neither and stops at boot. If your
group holds other accounts, give both files to the server's uid instead:

```sh
sudo chown 10001 tokens secret.key
sudo chmod 0600 tokens secret.key
```

The server also refuses to start when the database holds secrets written
under a different key. In that case, restore the original key file and start
the server again: it never overwrites or deletes secrets written under another
key. Each stored secret records the id of its key, the first 8 bytes of the
key's SHA-256 in hex, and the refusal names the ids it found. To find the id
of a key file:

```sh
openssl base64 -d -in secret.key | openssl dgst -sha256 -r | cut -c1-16
```

If the original key is lost, the secrets written under it cannot be recovered.
Delete those rows with the statement the refusal prints, which names their
key ids, and enter the credentials again.

At boot the server also decrypts every stored secret once. When one no longer
decrypts, for example after a damaged restore, it refuses to start and names
that secret. Restore the database from a backup taken before the damage, or
delete that row with the statement the refusal prints and enter its
credentials again.

The key cannot be rotated yet. To start over with a new key, delete the rows
from `deployment_secrets` and enter the secrets again.

The key protects dumps and backups of the database, not a database someone
can write to. Anyone who can write rows can put back an older copy of a row,
which still decrypts, and can already run commands on the server through
stored MCP server definitions.

On a self-host machine, a member who can use Code mode can read the key file,
the `tokens` file, and the database URL today: workspace terminals and coding
engines run as the server's uid and inherit its environment. `tokens` holds
every user's token, the administrators' included, so reading it lets a member
act as anyone. The Vault token file and provider environment variables are
exposed the same way. Until members' code sessions are kept away from the
deployment's secrets
([#3590](https://github.com/brightwave-inc/tidebreak/issues/3590)), give
self-host accounts only to people you would trust with those secrets.

Vault remains available. To use it instead, delete the
`TIDEBREAK_SECRET_KEY_FILE` line and the `secret.key` mount from the `server`
service, mount a Vault token file, and add the `TIDEBREAK_VAULT_*` variables
from the following section to `.env`. A deployment uses one or the other, and
secrets saved in one do not move to the other.

### Vault credential custody

To save provider, web-search, code-execution, and connected-app credentials
through Tidebreak in Vault, give the self-host server a HashiCorp Vault KV v2
mount. The server stores no Vault token in its database or boot configuration.
It reads the token from a mounted file for every Vault request, so an injector
or Vault Agent can rotate the file without restarting Tidebreak.

Tidebreak appends each internal credential key to the configured path. With a
mount of `secret` and a path of `tidebreak/production`, the normal credential
bundle lives under `secret/data/tidebreak/production/tidebreak.secret_bundle_v1`.
Grant the path wildcard because migration-safe fallback reads can address
other stable credential keys under the same prefix.

If the `secret` mount does not exist, enable KV v2:

```sh
vault secrets enable -path=secret kv-v2
```

Create a policy for one deployment path:

```hcl
path "secret/data/tidebreak/production/*" {
  capabilities = ["create", "read", "update", "delete"]
}
```

Attach that policy to the Vault identity used by the server. Configure your
Vault Agent, Kubernetes injector, or service supervisor to write the resulting
token to a file that only the Tidebreak process can read. Then set:

```sh
export TIDEBREAK_VAULT_ADDR=https://vault.internal.example
export TIDEBREAK_VAULT_TOKEN_FILE=/run/secrets/tidebreak-vault-token
export TIDEBREAK_VAULT_MOUNT=secret
export TIDEBREAK_VAULT_PATH=tidebreak/production
# Optional for Vault Enterprise or HCP Vault:
export TIDEBREAK_VAULT_NAMESPACE=platform/team-a
```

`TIDEBREAK_VAULT_ADDR` must use HTTPS. For local development, Tidebreak accepts
HTTP only when the host is a literal loopback address such as `127.0.0.1` or
`::1`; `localhost` does not qualify. The address cannot contain credentials, a
query, or a fragment, and Tidebreak refuses redirects. Keep the Vault token out
of environment variables and logs.

Vault KV v2 keeps version history according to the mount's retention settings.
Deleting a credential through Tidebreak deletes the latest version. If your
policy requires historical values to be destroyed, configure Vault retention
or destroy those versions through an operator-controlled Vault workflow.

If neither a key file nor Vault is configured, stored-secret reads return
unset so provider environment variables keep working. Attempts to save or
remove a credential fail and name `TIDEBREAK_SECRET_KEY_FILE`, or
`TIDEBREAK_VAULT_ADDR` and `TIDEBREAK_VAULT_TOKEN_FILE`, as the setup to add.

## Storage

Tidebreak keeps two kinds of data. PostgreSQL holds chats, projects, document
records, transcripts, the event journal, and the encrypted credentials. The
blob store holds the immutable bytes that those records point to: uploaded
documents, images, and generated artifacts.

The stock stack keeps blobs on local disk. `TIDEBREAK_BLOB_STORE_URL` defaults
to `file:///var/lib/tidebreak/blobs`, a directory on the `tidebreak-data`
volume. Local disk means the data volume holds part of your data: back it up
together with the database, as [Backup](#backup) describes.

The server creates the directory the first time it starts, readable by its own
user only. It writes every blob with mode `0600`, including in a directory you
created yourself, whose permissions it leaves alone. Uploads stage in the
directory's `_uploads/` folder, also private to the server's user, and publish
from there. At every boot, the server checks that it can write, delete, and
list in the directory. When it cannot, it refuses to start and names the
reason, such as `permission denied`. Later storage errors name their reason
the same way. Only the top level of the directory holds blobs, so a folder
the server cannot read there, such as `lost+found` at the root of a mounted
volume, does no harm.

A `file://` URL must name one absolute directory below the root:
`file:///absolute/path`, with no host, no `.` or `..` segments, no encoded
separators such as `%2F`, no query or fragment, and special characters
percent-encoded. The server refuses to start with anything else and says what
it expected.

### S3-compatible object storage

When you outgrow one machine, keep blobs in a bucket instead. Add the bucket
and an optional prefix to `.env`, with the standard AWS variables for
credentials and region, and then run `docker compose up -d`:

```sh
TIDEBREAK_BLOB_STORE_URL=s3://company-tidebreak/production
AWS_DEFAULT_REGION=us-east-1
AWS_ACCESS_KEY_ID=<your access key>
AWS_SECRET_ACCESS_KEY=<your secret key>
```

For an S3-compatible service, also set `AWS_ENDPOINT_URL_S3`. Leave out the
key variables when the machine has an instance role; the server then uses
instance credentials.

Switching backends does not copy blobs that already exist. Both backends name
a blob `<id>.blob` directly below their root, so to keep existing documents,
stop the server and copy every `.blob` file from `/var/lib/tidebreak/blobs` on
the data volume into the bucket prefix before you start it again.

Grant `s3:ListBucket` for the configured prefix. Grant `s3:GetObject`,
`s3:PutObject`, `s3:DeleteObject`, and `s3:AbortMultipartUpload` only for
objects below that prefix. The boot check writes and deletes a small object
below `_uploads/`, so the server needs those grants to start. Configure the
bucket to abort incomplete multipart uploads after a day. Also expire completed
objects in the `_uploads/` path below that prefix after a day because streamed
writes publish through that temporary path.

On local disk, the server does that cleanup itself. A failed upload removes
what it staged in `_uploads/` at once. A crash can still leave staged parts
behind, so at every boot the server deletes the entries in `_uploads/` that
are more than a day old.

## HTTPS and network exposure

The server serves plain HTTP and never terminates TLS. Compose publishes it on
`127.0.0.1:8080` of the machine and nowhere else, so nothing outside the
machine reaches it directly. That is the posture
[decision record 6](decisions/0006-self-host-deployment-plane-authorization.md)
states: TLS termination and network exposure belong to the infrastructure in
front of the server.

With a domain, the `caddy` service is that infrastructure. It starts when
`.env` sets `TIDEBREAK_DOMAIN` and `COMPOSE_PROFILES=tls`, which
`setup.sh --domain` writes. Caddy obtains a certificate for the domain from a
public certificate authority and renews it, redirects HTTP to HTTPS, and
proxies every request to the server, WebSocket upgrades included. For the
certificate to arrive, the domain's DNS record must point at the machine, and
ports 80 and 443 must be open to the internet. Caddy keeps the certificate and
its ACME account on the `caddy-data` volume, so keep that volume across
restarts.

Bearer tokens travel in two request headers: `Authorization`, from the CLI
and the API, and `Sec-WebSocket-Protocol`, which a browser sends on every
WebSocket upgrade. Token-file tokens do not expire, so neither header may reach
a log. Caddy writes no access log in this configuration, but its own log still
records the request, headers included, of every error it answers, such as a
`502` while the server restarts. `deploy/self-host/Caddyfile` therefore deletes
both headers from that log. If you add an access log to the site, give it the
same filter; the Caddyfile shows it.

To add a domain to a deployment that runs without one, add these lines to
`.env`, and then run `docker compose up -d`:

```sh
TIDEBREAK_DOMAIN=tidebreak.example.com
TIDEBREAK_PUBLIC_URL=https://tidebreak.example.com
COMPOSE_PROFILES=tls
```

Do not publish the server on `0.0.0.0` to make it reachable. The bearer check
still protects the deployment, but every token would then cross the network in
plain text.

### Your own reverse proxy

To terminate TLS on infrastructure you already operate, leave
`COMPOSE_PROFILES` unset so that Caddy stays off, and forward to
`127.0.0.1:8080`. The API is HTTP plus a WebSocket upgrade, so the proxy must
pass upgrades through.

Two things worth getting right:

- **The WebSocket credential travels in `Sec-WebSocket-Protocol`.** Browsers
  cannot set an `Authorization` header on a WebSocket upgrade, so on upgrade
  requests the server also accepts the token as
  `Sec-WebSocket-Protocol: tidebreak-token.<token>`, alongside the handshake
  subprotocol `tidebreak-v1`. Proxies log that header far more readily than
  they log `Authorization`. **Exclude `Sec-WebSocket-Protocol` from your proxy
  access logs**, and check your log shipper too — otherwise every user's
  bearer token ends up in plaintext log storage.
- **The proxy must be the only path in.** The bearer check is what protects
  the deployment; a directly reachable server port is a bypass of your TLS,
  not of the authentication.

An nginx sketch:

```nginx
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection $connection_upgrade;
    proxy_set_header Host $host;
    # Keep the WebSocket token out of the access log.
    proxy_read_timeout 3600s;
}
```

Configure the access-log format explicitly rather than relying on a default
that happens not to include request headers today.

## Code execution

This stack does not configure code execution, so the `exec` tool has no
backend on it. The macOS sandbox that local execution uses does not exist in a
Linux container, and the container backends need more than this stack gives
the server:

- The `exec` tool's Docker backend runs each chat's commands in a container of
  the pinned documents sandbox image, which it starts through the `docker`
  command and the host's Docker daemon. The server image carries no Docker
  CLI, and the stack does not mount the daemon's socket. A derived image that
  adds the CLI, with `/var/run/docker.sock` mounted and the socket's group
  added to the server, completes a run. This stack does not set that up:
  mounting the Docker socket gives the server root on the host. Code mode
  sessions run as the server's uid
  ([#3590](https://github.com/brightwave-inc/tidebreak/issues/3590)), so every
  member who can use Code mode would have root on the host too.
- `TIDEBREAK_CONTAINER_EXECUTION_ENABLED` routes background agent runs to
  sandbox containers. That backend publishes each sandbox's port on the host's
  loopback address and connects to `127.0.0.1`. Inside the server's container,
  `127.0.0.1` is the container itself, so the server cannot reach its
  sandboxes. Without a Docker CLI in the image, the backend also reports
  itself unavailable, and runs stay in the server process.

Keep the Docker socket out of the server container unless every account on
the deployment may have root on this machine.

## How the self-host profile works

Selecting `TIDEBREAK_PROFILE=self_host` changes five things about the server:

- **The store is PostgreSQL**, opened from `TIDEBREAK_DATABASE_URL`, and the
  binary must be built with tidebreak-server's `postgres` feature for the
  driver to exist at all.
- **Every request must name a user.** The desktop profile's per-launch bearer
  token authenticates nobody here. A standalone deployment uses the
  operator-managed token file, an OpenID Connect provider, or both; a
  deployment attached to Model Gateway validates its short-lived `tidebreak`
  resource tokens instead. Each resolves to a named principal carrying a role.
  Chats, projects, documents, transcripts, code workspaces, and event streams
  are owner-scoped to that principal.
- **Blob bytes live on local disk or in S3-compatible object storage**,
  selected by `TIDEBREAK_BLOB_STORE_URL`. PostgreSQL keeps the document catalog
  and references; the blob store keeps immutable source bytes, images, and
  artifacts.
- **Boot fails closed.** The server refuses to open the shared store unless
  it has exactly one valid authenticator — a Model Gateway, an OpenID Connect
  provider, or the token file — because a shared database never comes up
  behind an API that cannot tell its callers apart. The token file may sit
  beside OIDC as the bootstrap administrator and the CLI credential; the
  gateway and OIDC may not sit together.
- **Stored credentials are encrypted in PostgreSQL or kept in Vault KV v2.**
  The server never opens the desktop OS keychain. With a key file, it keeps
  them encrypted in its own database. With Vault configured, it keeps them in
  Vault. When neither is configured, provider environment variables remain
  available as read fallbacks, but deployment-plane credential writes and
  deletes fail with setup guidance.

The deployment posture is stated in
[decision record 6](decisions/0006-self-host-deployment-plane-authorization.md):
the server and its database run inside the operator's own network, and TLS
termination and network exposure belong to the operator's fronting
infrastructure. Tidebreak serves plain HTTP and never terminates TLS.

Settings are **deployment-scoped, not per-user** — enabled providers,
credentials, model roles, and policy configure the deployment itself, and
every administrator shares (and can change) them. The profile is for mutually
trusting users of one operator's deployment, not for adversarial tenants. See
the self-host section of
[how Tidebreak works](how-tidebreak-works.md#self-host) for the full statement
and for what is still integration work.

## Environment variables

Every variable below is read by the server or the CLI; nothing here is
aspirational.

| Variable | Required | Default | What it does |
| --- | --- | --- | --- |
| `TIDEBREAK_PROFILE` | yes | `desktop` | `self_host` (or `selfhost`) selects this profile. Anything else is desktop or a config error. |
| `TIDEBREAK_DATABASE_URL` | yes (self-host) | `DATABASE_URL` | PostgreSQL connection string for the shared store. On a Model Gateway managed machine the plane's `DATABASE_URL` stands in when this is unset. |
| `TIDEBREAK_BLOB_STORE_URL` | yes (self-host) | `file:///var/lib/tidebreak/blobs` in the compose stack | Where blob bytes live: a directory on this machine as `file:///absolute/path`, or an S3 bucket and optional prefix such as `s3://company-tidebreak/production`. The server creates a missing directory, readable by its own user only, and refuses a relative or unnormalized path. For S3, credentials, region, and an optional compatible endpoint come from standard `AWS_*` variables. |
| `AWS_DEFAULT_REGION`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_ENDPOINT_URL_S3`, `AWS_ALLOW_HTTP` | with an S3 blob store | AWS defaults | Configure AWS S3 or an S3-compatible endpoint. Keep `AWS_ALLOW_HTTP=false` outside isolated development networks. Role, web-identity, and container credential variables are also accepted. |
| `TIDEBREAK_AUTH_GATEWAY_URL` | one auth mode required | `GATEWAY_BASE_URL` | Public Model Gateway identity URL exposed to clients and, by default, used for live validation. HTTPS required except for loopback development. |
| `TIDEBREAK_PUBLIC_URL` | with Gateway or OIDC auth | `ADD_ON_PUBLIC_URL` | The machine's own public URL. Gateway credentials are bound to it, and the OIDC callback returns to it. On a Model Gateway managed machine the plane's `ADD_ON_PUBLIC_URL` stands in when this is unset (decision 0085). |
| `TIDEBREAK_AUTH_GATEWAY_VERIFIER_URL` | no | `TIDEBREAK_AUTH_GATEWAY_URL` | Optional server-to-server Gateway URL for principal validation when the public origin is not cluster-routable. Requires Gateway auth. |
| `TIDEBREAK_AUTH_TOKENS_FILE` | one auth mode required | `/run/tidebreak/tokens` in the compose stack | Path to the [token file](#token-file). Mutually exclusive with Gateway auth. Set it beside OIDC to name the first administrator and keep CLI access. |
| `TIDEBREAK_AUTH_OIDC_ISSUER` | one auth mode required | — | OpenID Connect issuer URL, whose `/.well-known/openid-configuration` the machine reads. HTTPS required except for loopback development. Mutually exclusive with Gateway auth; set all three OIDC variables or none. |
| `TIDEBREAK_AUTH_OIDC_CLIENT_ID` | with OIDC | — | OIDC client id. Register `<TIDEBREAK_PUBLIC_URL>/auth/oidc/callback` as its redirect URI. |
| `TIDEBREAK_AUTH_OIDC_CLIENT_SECRET` | with OIDC | — | OIDC client secret, used only for the server-to-server code exchange. It stays in process memory. |
| `TIDEBREAK_AUTH_OIDC_CLAIM` | no | `sub` | ID-token claim whose string value becomes the Tidebreak user id. Requires OIDC. |
| `TIDEBREAK_ADAPTER_BOOTSTRAP_TOKENS` | no | unset | Comma-separated service bearers allowed to start an external channel connect handshake. Each value must be 32–512 header-safe characters. Leave unset to disable connect start. To rotate without downtime, add the new value, move the adapter, then remove the old value. |
| `GH_TOKEN` or `GITHUB_TOKEN` | no | unset | Standalone forge token the server holds and lends per git operation. The agent child never inherits it. |
| `TIDEBREAK_GIT_BOT_LOGIN` | no | unset | GitHub login the token belongs to, so the UI can say whose account work lands as. |
| `TIDEBREAK_VAULT_ADDR` | required with Vault custody | — | Vault base URL. HTTPS is required except for literal loopback development. Setting any Vault option enables Vault configuration and requires this variable plus `TIDEBREAK_VAULT_TOKEN_FILE`. |
| `TIDEBREAK_VAULT_TOKEN_FILE` | required with Vault custody | — | Mounted file containing the Vault token. Tidebreak reads it for every request so rotation does not require a restart. |
| `TIDEBREAK_VAULT_MOUNT` | no | `secret` | KV v2 mount path. |
| `TIDEBREAK_VAULT_PATH` | no | `tidebreak` | Deployment-specific path below the mount. Tidebreak appends one encoded credential key. |
| `TIDEBREAK_VAULT_NAMESPACE` | no | unset | Vault Enterprise or HCP namespace sent as `X-Vault-Namespace`. |
| `TIDEBREAK_SECRET_KEY_FILE` | no | unset | Self-host only: file holding the 32-byte base64 key that encrypts stored credentials in the database. See [Secrets in the database](#secrets-in-the-database). Setting it together with the Vault variables refuses to start. |
| `TIDEBREAK_DATA_DIR` | yes (the image sets it) | `/var/lib/tidebreak` in the image | Instance lock, logs, per-turn scratch, harness installs, and, in the compose stack, the blob directory. Nothing defaults to the current directory: a self-host server started without it refuses to start and names the variable. |
| `HOME` | no | `/var/lib/tidebreak/home` in the image | Writable home for npm and the coding harnesses. The image keeps it on the data volume because a hosting plane may run the container as a uid with no passwd entry, which is otherwise handed `HOME=/`. The server creates it at boot. |
| `TIDEBREAK_LOG` | no | built-in policy | `tracing` filter directives, e.g. `debug` or `warn,tidebreak_server=trace`. An invalid spec falls back to the default. |
| `TIDEBREAK_DIAGNOSTICS_LOG` | no | `off,tidebreak_diagnostics=info` | `tracing` filter directives for the bounded structured JSONL log. See [Diagnostics](diagnostics.md). |
| `TIDEBREAK_MODEL` | no | built-in default | Default model name; also settable at runtime through settings or per chat. |
| `TIDEBREAK_MCP_CONFIG` | no | unset | External stdio MCP server configuration file loaded at boot. |
| `TIDEBREAK_CONTAINER_EXECUTION_ENABLED` | no | `false` | Routes background agent runs to sandbox containers. It does not work when the server itself runs in a container; see [Code execution](#code-execution). |
| `TIDEBREAK_CONTAINER_IMAGE` | no | server default | Agent container image, when the above is on. |
| `TIDEBREAK_RUNTIME_ENDPOINT` | remote sessions | unset | Model Gateway runtime endpoint slug used to provision remote code sessions. Requires Gateway authentication and `TIDEBREAK_RUNTIME_PROFILE`. |
| `TIDEBREAK_RUNTIME_PROFILE` | remote sessions | unset | Administrator-defined sandbox profile sent with every remote spawn. Requires `TIDEBREAK_RUNTIME_ENDPOINT`. |
| `TIDEBREAK_RUNTIME_CONCURRENCY_CAP` | no | `3` | Positive maximum number of live remote sandboxes each owner may hold. Restart Tidebreak after changing it. |
| `TIDEBREAK_RUNTIME_SPAWN_SPEND_CEILING_MICROUSD` | no | `5000000` | Positive per-spawn spend ceiling in micro-USD. Set `none` to leave this ceiling to the runtime profile. Restart Tidebreak after changing it. |
| `TIDEBREAK_RUNTIME_SESSION_SPEND_CEILING_MICROUSD` | no | `20000000` | Positive cumulative spend ceiling per remote session in micro-USD. Set `none` to remove Tidebreak's cumulative ceiling; the runtime profile still bounds each spawn. Restart Tidebreak after changing it. |
| `TIDEBREAK_EXTERNAL_PERMISSION_MODE` | no | `ask` | The permission mode a Slack-bound session starts in when it runs on this machine's own engine and the channel names none: `plan`, `ask`, `auto`, or `allow`. Sandbox sessions are always `allow`. Restart Tidebreak after changing it. |
| `TIDEBREAK_EXTERNAL_PERMISSION_CEILING` | no | the mode above | The most permissive mode a channel may ask for with `/tidebreak mode`. A request above it is refused by name. Must not be below `TIDEBREAK_EXTERNAL_PERMISSION_MODE`. Restart Tidebreak after changing it. |
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `XAI_API_KEY`, `GEMINI_API_KEY`, `FIREWORKS_API_KEY`, `TOGETHER_API_KEY` | no | unset | Fallback provider credentials, consulted when no stored credential exists for that provider, including when neither a key file nor Vault is configured. |
| `ANTHROPIC_BASE_URL`, `OPENAI_BASE_URL`, `OPENAI_COMPATIBLE_BASE_URL`, `OLLAMA_BASE_URL` | no | unset | Fallback provider endpoints, consulted when no base URL is stored for that provider. Point a provider at a compatible endpoint from your chart or compose file instead of setting it after first boot. Use HTTPS; Ollama also accepts HTTP on a loopback address. An unusable value is ignored, and the provider keeps its built-in endpoint. |
| `TIDEBREAK_LISTEN_ADDR` | no | loopback, ephemeral port | Self-host only: the address and port the API binds, e.g. `0.0.0.0:8080`. The desktop profile refuses to boot with it set — that profile's loopback binding is what its per-launch token assumes. The image sets it to `0.0.0.0:8080` so the container is reachable at a known port. Engine children are still handed a loopback address on that port, and the engine relay and git-credential routes answer loopback peers only. |
| `TIDEBREAK_UI_DIST` | no | unset | A built desktop renderer bundle to serve to browsers; see [Opening the machine in a browser](#opening-the-machine-in-a-browser). The image sets it to the bundle it carries. Unset, the server serves no pages and an unknown path answers `404`. The server refuses to start if the directory holds no `index.html`. |

### Compose variables

`setup.sh` writes these to `.env`, and `docker-compose.yml` reads them. Every
other line in `.env` reaches the server as an environment variable, so most
settings in the preceding table go there too. The compose file sets the
profile, the database URL, the token and key file paths, the data directory,
and the listen address itself; to change those, edit `docker-compose.yml`.

| Variable | Default | What it does |
| --- | --- | --- |
| `TIDEBREAK_VERSION` | none; required | The release of `ghcr.io/brightwave-inc/tidebreak-server` that `up` pulls: 0.117.0 or later for the local-disk default. |
| `POSTGRES_PASSWORD` | none; required | The database password. Only PostgreSQL and the server use it. |
| `TIDEBREAK_HOST_GID` | `10001` | The host group that owns `tokens` and `secret.key`. Compose adds it to the server so the server can read them. |
| `TIDEBREAK_DOMAIN` | unset | The domain Caddy serves over HTTPS. |
| `COMPOSE_PROFILES` | unset | `tls` starts the `caddy` service. Set it together with `TIDEBREAK_DOMAIN`. |
| `COMPOSE_FILE` | `docker-compose.yml` | `docker-compose.yml:docker-compose.build.yml` builds the server image from this checkout for every command. |
| `TIDEBREAK_BLOB_STORE_URL` | `file:///var/lib/tidebreak/blobs` | Overrides the blob store; see [Storage](#storage). |
| `TIDEBREAK_LOG` | `info` | The server's log filter. |

## What the image provides

The image builds the CLI with `--no-default-features` and
`tidebreak-server/postgres`. It excludes the OS keychain and its Linux D-Bus
dependencies. Standard CLI and desktop builds keep persistent OS credentials.
A build without `keychain` refuses the desktop profile at startup.

Code mode runs agents on the machine, so the image carries the tools it
spawns. Read this before you swap in a base image of your own: the server
does not install any of it, and reports a missing piece as an unavailable
engine rather than an installation prompt.

| Tool | Version | Why it is there |
| --- | --- | --- |
| Managed Node runtime | 24.21.0, at `/opt/tidebreak/node/24.21.0` | The runtime every harness install runs `npm` from. |
| `git` | 2.39.5 (Debian bookworm) | Clone, worktree, checkpoint, commit, and push. |
| `gh` | 2.98.0 (the project's own release) | Pull-request create, status, review reads, and merge. |
| `curl`, `ca-certificates` | Debian bookworm | The container healthcheck and the system trust store. |

The Node runtime is the strict one. The server accepts it from exactly one
path, `$TIDEBREAK_DATA_DIR/tools/node/<version>`, and only when that directory
holds `bin/node`, `bin/npm`, and an `installed.json` naming the version and
the SHA-256 of the official nodejs.org artifact it was unpacked from. Nothing
is scanned and `PATH` is never consulted, so a Node installed elsewhere in
your image does not count. The image keeps its copy under `/opt` and the
container entrypoint links the data directory at it on every start, because
that path sits under a volume mount and anything the image layer puts there
disappears the moment an operator mounts one.

Harness packages install into the data directory on demand, at the versions
Tidebreak pins, so give the volume room for them — a few hundred megabytes
per engine.

Two things the image deliberately does not decide for you:

- **A GitHub identity.** Set `GH_TOKEN` (or `GITHUB_TOKEN`) in the server's
  environment. The server holds it and lends it per git operation through
  the same seam a hosted machine uses; the agent child never sees the
  token. Everyone on the deployment acts as that one account. Set
  `TIDEBREAK_GIT_BOT_LOGIN` to the GitHub login the token belongs to so the
  UI can say whose account work lands as. Per-user GitHub identity is a
  hosted-machine path, not this one.
- **A commit identity.** `git commit` needs a name and an email, and the image
  invents neither. Set `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`,
  `GIT_COMMITTER_NAME`, and `GIT_COMMITTER_EMAIL` in the server's environment,
  or mount a `.gitconfig` into the container's home directory. Without one,
  commits from code mode fail and the checkpoint history is what still works.

The image ships no SSH client and no known-hosts file, so clone over HTTPS.
An SSH clone URL fails.

### The end-to-end fixture image

`ghcr.io/brightwave-inc/tidebreak-server-e2e:main` is the same Dockerfile
built with `--build-arg CARGO_PROFILE=dev`. That build carries the scripted
harness, an engine that plays a JSON script of events from
`TIDEBREAK_SCRIPTED_HARNESS` instead of running a model, so an integration
lane can drive a real machine through connect, a turn, and an approval with
nothing but the machine and a database. Model Gateway's Slack adapter lane is
the consumer. The image is a test fixture: it is never attested, never
versioned, and never a candidate for a managed machine. Do not deploy it.

## Bring your own image

You may replace the published server image with one you build, as long as
the server still finds every path it checks. The Dockerfile comments in
`deploy/self-host/Dockerfile` are the contract. Keep all of the following:

- **Managed Node at one path.** The server accepts Node only from
  `$TIDEBREAK_DATA_DIR/tools/node/<version>`. That directory must contain
  `bin/node`, `bin/npm`, and `installed.json` naming the version and the
  SHA-256 of the official nodejs.org artifact the tree was unpacked from.
  Nothing is scanned and `PATH` is never consulted, so a Node you install
  elsewhere does not count.
- **The data directory.** The image sets `TIDEBREAK_DATA_DIR` to
  `/var/lib/tidebreak`. A volume mounted there hides whatever the image
  layer put underneath it.
- **The entrypoint's link step.** The image keeps its Node copy under
  `/opt/tidebreak/node/<version>`. `tidebreak-entrypoint` links the data
  directory at that copy on every start. Unpack Node into the data
  directory at build time and the link is gone the moment you mount a
  volume.
- **The `serve` command.** The entrypoint runs `tidebreak serve`. If you
  replace the entrypoint, or override it to reach the binary directly, pass
  `serve` yourself: a bare `tidebreak` prints help and exits.
- **The healthcheck.** The image probes `http://127.0.0.1:8080/healthz`
  with `curl`. Keep `curl` and that listen address, or replace the
  healthcheck with an equivalent probe of `/healthz`.
- **The non-root user.** The server runs as uid `10001` / gid `10001`
  (`tidebreak`). Own the data directory and home so that user can write
  them. A hosting plane may still run a different non-root uid; `HOME`
  stays on the data volume for that case.

You may add packages, compilers, and language runtimes on top of that
contract. You may also change the Debian snapshot or the Node pin, if you
keep `installed.json` in lockstep with the tree you unpack. Do not drop
the link step, the healthcheck, or the unprivileged user.

## Toolchain bundles

The default image carries managed Node, `git`, and `gh` only. A machine
session that runs `cargo test` on that image fails because `cargo` is not
there. Optional bundles install extra toolchains at image build:

```sh
docker build --build-arg TOOLCHAINS=rust,python \
  -f deploy/self-host/Dockerfile \
  -t tidebreak-self-host \
  .
```

To use such an image with the compose stack, build it through the build file,
and then start the stack with the same two files:

```sh
docker compose -f docker-compose.yml -f docker-compose.build.yml build --build-arg TOOLCHAINS=rust,python
docker compose -f docker-compose.yml -f docker-compose.build.yml up -d
```

`TOOLCHAINS` is a comma-separated list. The default is empty and installs
nothing extra. Known names are `rust`, `python`, `go`, and `jvm`. An
unknown name fails the build and prints the name. Pins, SHA-256 digests,
and Debian package versions live in
[`deploy/self-host/TOOLCHAINS.md`](../deploy/self-host/TOOLCHAINS.md). The
image label `io.tidebreak.toolchains` records the argument you passed, so
the digest's provenance states which bundles it carries.

| Bundle | What you get |
| --- | --- |
| `rust` | rustup 1.27.1, stable toolchain 1.97.1, cargo, clippy, rustfmt, and Debian `build-essential` 12.9 so crates can link |
| `python` | Debian `python3` 3.11.2-1+b1, `python3-pip` 23.0.1+dfsg-1, `python3-venv` 3.11.2-1+b1 |
| `go` | Go 1.25.1 from the official release tarball, SHA-256 verified before unpack |
| `jvm` | Debian OpenJDK 17 headless 17.0.20+8-1~deb12u1 and Maven 3.8.7-1 |

When a workspace setup script or a test quick action fails with
`command not found` for `cargo`, `python3`, `go`, `mvn`, or `java`, the
turn names the missing tool and points you at this page.

If this machine has a Model Gateway runtime endpoint, run the suite in a
sandbox child instead of stuffing every compiler into the server image.
The runtime profile's image is the toolchain for that path.

## Opening the machine in a browser

The image carries the Tidebreak desktop app's renderer, and the server serves
it at the machine's own address: a browser tab at `TIDEBREAK_PUBLIC_URL`
lands on the same app the desktop runs, attached to this machine. Pages are
served for navigations only — a request for an unknown route with a JSON
`Accept` still answers `404`, and every API route is matched ahead of the
bundle — so the API contract does not change.

However a tab signs in, it ends the same way: the bearer arrives in the URL
fragment, which no server and no access log sees, and the page takes it out
of the address before the router runs. It lives in that tab's memory for its
hour and nowhere else — never a cookie, never localStorage, never disk. A
link to a session keeps its route through sign-in, so you land on the session
you opened rather than the root. Which path a machine offers is its own to
say: the page reads `/auth/discovery`, which is public and needs no bearer,
and shows the one screen that machine can act on. See
[decision 0087](decisions/0087-standalone-browser-sign-in.md).

**With a Model Gateway** (`TIDEBREAK_AUTH_GATEWAY_URL`), the bearer comes
from the console's Manage action: the console sends the browser to the
machine's `/auth/handoff` route with a one-time code, and the machine
exchanges that code with the gateway server to server. A tab that opens the
address directly, outlives its bearer, or arrives with a code that has
already been used shows a sign-in screen that sends you back through the
console.

**With a token file** (`TIDEBREAK_AUTH_TOKENS_FILE`), write one whitespace-separated mapping per line as `name token`, with an optional third field. Use `admin` for a person who may configure the deployment, or `service` for a member that owns automated sessions and never signs in. Do not combine `admin` and `service`; keep at least one person marked `admin`. The page asks you to
paste your token. It probes the token against an authenticated read on the
machine first, so a wrong one leaves you on the same screen with the
refusal instead of a broken session. A token is as strong as the file it came
from; whoever maintains the roster decides who holds one.

**With an OpenID Connect provider**, set `TIDEBREAK_AUTH_OIDC_ISSUER`,
`TIDEBREAK_AUTH_OIDC_CLIENT_ID`, and `TIDEBREAK_AUTH_OIDC_CLIENT_SECRET`, and
register `<TIDEBREAK_PUBLIC_URL>/auth/oidc/callback` as the client's redirect
URI. The page then shows one button. The machine runs authorization code with
PKCE itself: it starts the flow at `/auth/oidc/start`, and at
`/auth/oidc/callback` it checks the `state` against a flow it started,
exchanges the code with the PKCE verifier, and validates the ID token against
the issuer's discovery document and keys — signature, issuer, expiry, the
client id as audience, and the flow's nonce. Only then does it mint its own
bearer, good for one hour. Anything that does not check out signs nobody in.

`TIDEBREAK_AUTH_OIDC_CLAIM` names the ID-token claim whose string value
becomes the Tidebreak user id; it defaults to `sub`, the one claim every
issuer sends. Point it at `email` or `preferred_username` when you want
readable owner ids, and know that the mapping is the identity: a claim the
provider stops sending, or a value that changes, signs that person in as
nobody rather than as someone else. Tidebreak asks for the scope that claim
needs (`email` or `profile`) alongside `openid`, so grant it to the client.

OIDC never makes anyone an administrator. Keep `TIDEBREAK_AUTH_TOKENS_FILE`
set beside it: that file is where the first administrator comes from, and it
is what CLI and script access uses. What you cannot do is combine OIDC with
`TIDEBREAK_AUTH_GATEWAY_URL` — a machine signs browsers in through one
identity provider, and setting both is a boot error that names the two
variables.

Everything that reaches the reader's own computer — connected folders, tool
calls on the local machine, saving files locally, computer use — is
unavailable in a browser tab, as it is for any remote attachment.

To run the image without pages, unset `TIDEBREAK_UI_DIST`.

## Run the Slack adapter beside this machine

The adapter is a shared, stateful service. It is not part of the server
image. You run it next to a standalone machine when you want mentions and
DMs in your own Slack app to drive sessions on that machine. The image is `ghcr.io/brightwave-inc/tidebreak-slack-adapter`
(tags `v<version>`). An anonymous pull is refused; you need access to that
package.
It listens on 8080. It needs its own PostgreSQL (`DATABASE_URL`), a
token-sealing key, Slack credentials, and a machine directory. It does not
need Model Gateway variables. One adapter instance uses one Slack workspace's
bot token. Create and install a custom Slack app in that workspace; a distributed
OAuth app is not required for this setup.

A Compose example that starts the machine, PostgreSQL, the adapter, and the
adapter's database lives in
[`deploy/compose/slack/`](../deploy/compose/slack/). Use that directory; do
not bolt the adapter onto `deploy/self-host/` without giving it its own
store.

### Slack app to import

Create a Slack app from a manifest. Give the bot scopes that let it read
mentions and DMs, post in threads, add and read reactions, and read channel
membership. Channel history scopes let the agent read thread context; `files:read`
lets it retrieve supported attachments. Point event subscriptions, slash commands,
and interactivity at `https://<your adapter host>/webhooks/slack`. The adapter
handles all three payload types on that route. Reinstall the app after changing
its scopes.

A sketch (replace the host):

```yaml
display_information:
  name: Tidebreak
features:
  bot_user:
    display_name: Tidebreak
    always_online: true
  slash_commands:
    - command: /tidebreak
      url: https://<your adapter host>/webhooks/slack
      description: Help, repository defaults, and session commands
      should_escape: false
oauth_config:
  scopes:
    bot:
      - app_mentions:read
      - assistant:write
      - commands
      - channels:history
      - groups:history
      - mpim:history
      - files:read
      - users:read
      - im:history
      - im:read
      - im:write
      - chat:write
      - reactions:read
      - reactions:write
      - channels:read
      - groups:read
      - mpim:read
settings:
  interactivity:
    is_enabled: true
    request_url: https://<your adapter host>/webhooks/slack
  event_subscriptions:
    request_url: https://<your adapter host>/webhooks/slack
    bot_events:
      - app_mention
      - message.channels
      - message.groups
      - message.mpim
      - message.im
      - app_home_opened
      - team_join
      - user_change
      - app_uninstalled
      - tokens_revoked
  org_deploy_enabled: false
  socket_mode_enabled: false
```

The adapter receives events and slash commands over the HTTPS request URLs
above; it does not use socket mode, so no app-level token is needed.

Put `SLACK_BOT_TOKEN` and `SLACK_SIGNING_SECRET` in `slack.env` beside the
Compose file. Keep that file off the machine's `.env` so a restart of one
side does not leak the other's secrets.

### Bootstrap bearer the machine expects

The connect handshake is not anonymous. Set
`TIDEBREAK_ADAPTER_BOOTSTRAP_TOKENS` on the machine to one or more
32–512 character bearers (comma-separated). Generate one with
`openssl rand -hex 32`. The adapter sends that bearer only to start
connect. Leave the variable unset and the machine refuses connect start.

Put the same value in the machine directory as `bootstrap_token`. To rotate
without downtime, add the new value on the machine, move the adapter, then
remove the old value.

### Service line for channel sessions

Channel sessions need a durable owner that never signs in. In the token
file, add a `service` line
([decision 0089](decisions/0089-service-principals.md)):

```text
alice  <person token>   admin
slack  <service token>  service
```

A service line does not satisfy the "at least one admin" check. Do not
combine `admin` and `service` on one line.

### Machine directory (file or environment)

The adapter finds your machine from a JSON directory of this shape:

```json
{
  "machines": {
    "<ref>": {
      "kind": "standalone",
      "base_url": "http://tidebreak:8080",
      "public_url": "https://<your machine host>",
      "bootstrap_token": "<the bearer the machine lists in TIDEBREAK_ADAPTER_BOOTSTRAP_TOKENS>"
    }
  },
  "defaults": {
    "<Slack workspace id>": "<ref>"
  }
}
```

`base_url` is how the adapter reaches the machine on the Compose network.
`public_url` is how a person opens the machine in a browser to approve the
workspace grant.

Pass that document in either of two ways:

- **File.** Mount it into the adapter container and point
  `SLACK_ADAPTER_MACHINES_FILE` at it, as the Compose example does with
  `./machines.json`. Adapter images from gateway release v0.1.0-alpha.195
  read the file.
- **Environment.** Set `SLACK_ADAPTER_MACHINES` to the same JSON string.

`GET /health/setup` on the adapter reports `ready` and `missing`. Use it
before you install the Slack app.

### Approve the workspace grant

After a first mention, the adapter starts a connect handshake. Open the
machine in a browser at `TIDEBREAK_PUBLIC_URL`, sign in as an
administrator, and approve the Slack workspace grant on that page. The
approval names the workspace being linked. Until you approve it, channel
sessions do not run.

Once the grant is live, invite the app into a channel and mention it with a task.
No repository is required. A channel default set with
`/tidebreak repo set owner/name` is optional; it selects where work starts.
The configured GitHub App controls repository access across the instance.
Connected channels need no separate repository approval.

To set a channel's harness, model, automatic replies, or instructions, open its
Configure link in Tidebreak. New sessions use its harness, model, and instructions;
automatic replies apply to existing threads too. `/tidebreak help` lists commands.
See [Slack sessions](slack-sessions.md) for identity and repository choice.

### When Gateway hosts the machine

Use Gateway's Slack setup page to create the custom app manifest, supply the bot
token and signing secret, connect the GitHub identities, and pair the adapter
with Tidebreak. Use its runtime provisioning action to configure a sandbox
profile and the matching supervised-agent image. This path still installs a
custom app in one Slack workspace; it does not require distributing an OAuth app.

Tidebreak stores channel behavior preferences. Gateway controls credentials,
GitHub App access, model and subscription admission, sandbox resources, and spend
limits. Configure shared repository access once through the GitHub App
installation; changing channel preferences never expands that access.

For a repository-less Slack session with no chosen harness, Tidebreak uses the
configured runtime's admitted default engine. Without a runtime, or with a legacy
runtime that declares no engines, it uses Internal on the machine. Explicit
Internal also stays on the machine. Invalid declared runtime settings refuse
the session instead of silently moving
it. Pin the supervised-agent image and coordinate its version with the server;
[the runtime guide](slack-sessions.md#packaged-sandbox-runtime) describes upgrades.

## Backup

Back up three things: PostgreSQL, the blobs, and `secret.key`. PostgreSQL holds
chats, projects, document records, transcripts, the event journal, and the
encrypted credentials. The blobs hold the immutable bytes those records
reference, in `/var/lib/tidebreak/blobs` on the `tidebreak-data` volume.
Restore the database and the blobs from the same backup window, or documents
point at bytes that are missing.

To copy the database and the blobs out of a running stack, run:

```sh
docker compose exec -T postgres pg_dump -U tidebreak tidebreak | gzip > tidebreak-$(date +%F).sql.gz
docker compose exec -T server tar czf - -C /var/lib/tidebreak blobs > tidebreak-blobs-$(date +%F).tar.gz
```

To restore on a machine with no Tidebreak volumes yet, prepare the directory
with the same `.env`, `tokens`, and `secret.key`, and then do the following:

1. Start PostgreSQL alone, and wait until `docker compose ps` reports it
   healthy:

   ```sh
   docker compose up -d postgres
   ```

2. Load the database dump:

   ```sh
   gunzip -c tidebreak-<date>.sql.gz | docker compose exec -T postgres psql -U tidebreak tidebreak
   ```

3. Unpack the blobs into the data volume:

   ```sh
   docker compose run --rm --no-deps -T --entrypoint tar server xzf - -C /var/lib/tidebreak < tidebreak-blobs-<date>.tar.gz
   ```

4. Start the rest of the stack:

   ```sh
   docker compose up -d
   ```

With blobs in a bucket, back up the bucket prefix instead of the volume, from
the same window as the database.

`.env`, `tokens`, and `secret.key` live beside `docker-compose.yml`, not in a
volume. Back them up separately as secrets, and keep `secret.key` away from the
database backups: a database backup alone reveals no stored credential, and
one stored with its key reveals all of them. See
[Secrets in the database](#secrets-in-the-database).

## Upgrading

The server image and the files in `deploy/self-host/` change together, so move
both to the same release. To upgrade, do the following in `deploy/self-host/`:

1. Take a [backup](#backup).
2. Check out the release's tag, so `docker-compose.yml` matches the image:

   ```sh
   git fetch --tags
   git checkout v<version>
   ```

3. Set `TIDEBREAK_VERSION=<version>` in `.env`. If `.env` sets `COMPOSE_FILE`
   because no release could keep blobs on local disk when you set up, delete
   that line now.
4. Run `./setup.sh` again. It keeps your files, and adds any setting that the
   new `docker-compose.yml` needs and your `.env` lacks, such as
   `TIDEBREAK_HOST_GID` for a stack set up before `setup.sh` existed.
5. Pull the image and recreate the stack:

   ```sh
   docker compose pull
   docker compose up -d
   ```

A stack from before local-disk storage keeps its blobs in S3 through
`TIDEBREAK_BLOB_STORE_URL` in `.env`, and keeps doing so after the upgrade.

To build from source instead, pull the checkout and rebuild with the build
file, or run `docker compose up -d --build` when `.env` sets `COMPOSE_FILE`:

```sh
git pull
docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --build
```

The runtime image pins both Debian base images by digest and installs its
runtime package set from a dated Debian snapshot with exact direct versions.
The two artifacts it fetches from outside Debian — the managed Node runtime
and `gh` — are pinned by version and SHA-256 and verified before they are
unpacked. That keeps a rebuild of one commit from silently picking up
different `curl`, CA-certificate, Node, `gh`, or transitive package bytes.
Updating the snapshot date and the pins is therefore an explicit
dependency-maintenance change rather than an incidental effect of rebuilding.

The server applies its own schema migrations on boot. Each schema change is an
appended migration that upgrades the database in place
([decision record 61](decisions/0061-schema-changes-are-migrations.md)), and
hosted PostgreSQL upgrades the same way.

You own PostgreSQL backups. The desktop app copies its local SQLite database
before a migration, but the server never copies PostgreSQL. Take a
[backup](#backup) before every upgrade, and do not keep irreplaceable data here
without one.

To roll back, restore the backup you took before the upgrade into a fresh
database, then start the older version's image against it. An older server
does not open a database that a newer one migrated. It stops at boot with this
message:

```text
This Tidebreak profile was written by a newer version. Install that version or later, or restore a backup.
```

## What is not supported yet

Self-host is a real profile with real gaps. Rather than restate them here,
read the self-host section of
[how Tidebreak works](how-tidebreak-works.md#self-host) — it is the canonical
account. In summary, and each of these is a reason not to put irreplaceable
data in a self-host deployment yet:

- Tidebreak enforces one active server process per PostgreSQL database through
  a dedicated advisory lease. A second process refuses boot even when it uses
  another data directory. Horizontal multi-process serving remains unsupported
  until every process-local worker and live-delivery path has a distributed
  owner.

One more, specific to this packaging:

- Code execution does not run on this stack as shipped. The `exec` tool has
  no backend here; see [Code execution](#code-execution) for what the
  container backends need.
