# Self-hosting Tidebreak

The Tidebreak desktop app is the primary product: it runs the whole engine
locally, keeps state in SQLite under your own home directory, and needs no
server. **Self-hosting is for something else** — a team that wants one shared
deployment inside its own network (a VM, a VPC, an office server), with named
users, a shared PostgreSQL database, and shared provider credentials the
operator manages.

This guide covers running that deployment from the packaging in
`deploy/self-host/`. It describes only behavior verified in the code on this
branch; where something is not built yet, it says so.

## What the self-host profile is

Selecting `TIDEBREAK_PROFILE=self_host` changes three things about the server:

- **The store is PostgreSQL**, opened from `TIDEBREAK_DATABASE_URL`, and the
  binary must be built with tidebreak-server's `postgres` feature for the
  driver to exist at all.
- **Every request must name a user.** The desktop profile's per-launch bearer
  token authenticates nobody here; credentials come from an operator-managed
  token file and resolve to a named principal carrying a role. Chats, projects,
  documents, transcripts, and the event stream are owner-scoped to that
  principal.
- **Boot fails closed.** The server refuses to open the shared store unless
  `TIDEBREAK_AUTH_TOKENS_FILE` is set and loads cleanly — a shared database
  never comes up behind an API that cannot tell its callers apart, or that
  nobody is empowered to configure.

The deployment posture is stated in
[decision record 4](decisions/0004-self-host-deployment-plane-authorization.md):
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

## Prerequisites

- Docker with Compose v2.
- A machine that can reach your model provider's API.
- Somewhere private to keep the tokens file and the database password.

## Generating tokens

The token file is the credential-to-principal map, and it is also where roles
are managed — there is deliberately no UI for that. One line per token,
whitespace-separated, `#` comments and blank lines ignored:

```text
# user-id  token                                                             role
alice  4f9c0e9b2d5a4c1e8f7b6a5d4c3b2a1f9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b  admin
bob    0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
```

Generate each token with:

```sh
openssl rand -hex 32
```

Rules the loader enforces:

- Tokens are at least **32 characters** drawn from `[A-Za-z0-9._~-]`. Thirty-two
  random bytes in hex gives 64 characters, comfortably over the floor.
- The optional third field is the user's role. `admin` puts them on the
  deployment plane; an absent field means member, and anything else is a parse
  error rather than a silent demotion.
- **At least one line must say `admin`**, or the file fails to load and the
  server does not start. A deployment nobody is empowered to configure must
  not exist.
- A user's lines must agree about their role. A file that says both fails to
  load.
- One user may hold several tokens, which is how rotation works. A token may
  name only one user, and a duplicate token fails the load.

### What a member can and cannot do

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
[decision record 4](decisions/0004-self-host-deployment-plane-authorization.md).

Give `admin` only to the people who actually administer the deployment: MCP
server definitions spawn processes on the host, and the provider credentials
are shared.

Keep the file `0600` and owned by whoever runs the stack:

```sh
umask 077
printf 'alice %s admin\n' "$(openssl rand -hex 32)" > deploy/self-host/tokens
```

## Environment variables

Every variable below is read by the server or the CLI; nothing here is
aspirational.

| Variable | Required | Default | What it does |
| --- | --- | --- | --- |
| `TIDEBREAK_PROFILE` | yes | `desktop` | `self_host` (or `selfhost`) selects this profile. Anything else is desktop or a config error. |
| `TIDEBREAK_DATABASE_URL` | yes (self-host) | — | PostgreSQL connection string for the shared store. |
| `TIDEBREAK_AUTH_TOKENS_FILE` | yes (self-host) | — | Path to the token file above. Absent, boot fails before the store opens. |
| `TIDEBREAK_DATA_DIR` | no | `./.tidebreak` | Instance lock, logs, per-turn scratch. Durable state lives in PostgreSQL, not here. |
| `TIDEBREAK_LOG` | no | built-in policy | `tracing` filter directives, e.g. `debug` or `warn,tidebreak_server=trace`. An invalid spec falls back to the default. |
| `TIDEBREAK_MODEL` | no | built-in default | Default model name; also settable at runtime through settings or per chat. |
| `TIDEBREAK_MCP_CONFIG` | no | unset | External stdio MCP server configuration file loaded at boot. |
| `TIDEBREAK_CONTAINER_EXECUTION_ENABLED` | no | `false` | Enables the container code-execution backend. The compose stack does not configure one. |
| `TIDEBREAK_CONTAINER_IMAGE` | no | server default | Agent container image, when the above is on. |
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `XAI_API_KEY`, `GEMINI_API_KEY`, `FIREWORKS_API_KEY`, `TOGETHER_API_KEY` | no | unset | Fallback provider credentials, consulted when no credential is stored for that provider. A container has no OS keychain, so this is how a self-host deployment supplies model keys. |
| `TIDEBREAK_LISTEN_ADDR` | no | loopback, ephemeral port | Self-host only: the address and port the API binds, e.g. `0.0.0.0:8080`. The desktop profile refuses to boot with it set — that profile's loopback binding is what its per-launch token assumes. The image sets it to `0.0.0.0:8080` so the container is reachable at a known port. |

## Compose quickstart

```sh
cd deploy/self-host

# 1. Tokens. At least one line must be an admin, or the server will not boot.
umask 077
printf 'alice %s admin\n' "$(openssl rand -hex 32)" > tokens

# 2. Database password and provider key.
cat > .env <<'EOF'
POSTGRES_PASSWORD=<a long random string>
ANTHROPIC_API_KEY=<your key>
EOF
chmod 600 .env

# 3. Build and start. The first build compiles the workspace and is slow.
docker compose up -d --build

# 4. Confirm it is up.
curl -fsS http://127.0.0.1:8080/healthz     # -> ok
```

`/healthz` is the one unauthenticated route. Everything else needs
`Authorization: Bearer <token>` with a token from your file.

The stack publishes `127.0.0.1:8080` deliberately. Nothing in
`docker-compose.yml` terminates TLS, and the database port is not published
at all.

## Putting it behind a reverse proxy

Terminate TLS in front of the server, on infrastructure you already operate,
and forward to `127.0.0.1:8080`. The API is HTTP plus a WebSocket upgrade, so
the proxy must pass upgrades through.

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

## Backup

**Back up the `tidebreak-postgres` volume.** All durable state — chats,
projects, documents, transcripts, the event journal — lives in PostgreSQL.
The `tidebreak-data` volume holds only the instance lock, logs, and per-turn
scratch, and is safe to lose.

A logical dump is the simplest form:

```sh
docker compose exec -T postgres pg_dump -U tidebreak tidebreak | gzip > tidebreak-$(date +%F).sql.gz
```

Restore into a fresh, empty database before starting the server against it.

The tokens file and `.env` are not in either volume. Back them up separately,
as secrets.

## Upgrading

```sh
git pull
docker compose up -d --build
docker image prune -f     # optional
```

The runtime image pins both Debian base images by digest and installs its small
runtime package set from a dated Debian snapshot with exact direct versions.
That keeps a rebuild of one commit from silently picking up different `curl`,
CA-certificate, or transitive package bytes. Updating the snapshot date and
package pins is therefore an explicit dependency-maintenance change rather
than an incidental effect of rebuilding.

The server applies its own schema migrations on boot. Take a database backup
before an upgrade: Tidebreak is pre-1.0 and persisted formats may change
between versions (see
[decision record 2](decisions/0002-pre-v1-schema-and-persisted-format-mutability.md)).

## What is not supported yet

Self-host is a real profile with real gaps. Rather than restate them here,
read the self-host section of
[how Tidebreak works](how-tidebreak-works.md#self-host) — it is the canonical
account. In summary, and each of these is a reason not to put irreplaceable
data in a self-host deployment yet:

- Document and blob PostgreSQL parity is not comprehensively tested (the
  durable turn state machine is what CI exercises against PostgreSQL).
- Remote secret custody is future work, and it shows up immediately in a
  container: there is no OS keychain behind the credential store, so the
  deployment-plane routes that write or read provider and web-search
  credentials answer `500 web search credential storage is unavailable`
  rather than storing anything. Supply model credentials through the
  environment instead (see the table above) — which also means they are
  visible to anyone who can inspect the container.
- Object storage is not wired.
- Multi-process ownership is not wired: run exactly one server process against
  a database. The instance lock only guards one data directory, not the shared
  store.

One more, specific to this packaging:

- Code execution is not configured by this stack. `exec` needs a backend, and
  none of the container backends is set up here.
