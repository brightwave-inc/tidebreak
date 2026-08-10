# Self-hosting OpenWave

The OpenWave desktop app is the primary product: it runs the whole engine
locally, keeps state in SQLite under your own home directory, and needs no
server. **Self-hosting is for something else** — a team that wants one shared
deployment inside its own network (a VM, a VPC, an office server), with named
users, a shared PostgreSQL database, and shared provider credentials the
operator manages.

This guide covers running that deployment from the packaging in
`deploy/self-host/`. It describes only behavior verified in the code on this
branch; where something is not built yet, it says so.

## What the self-host profile is

Selecting `OPENWAVE_PROFILE=self_host` changes three things about the server:

- **The store is PostgreSQL**, opened from `OPENWAVE_DATABASE_URL`, and the
  binary must be built with openwave-server's `postgres` feature for the
  driver to exist at all.
- **Every request must name a user.** The desktop profile's per-launch bearer
  token authenticates nobody here; credentials come from an operator-managed
  token file and resolve to a named principal. Chats, projects, documents,
  transcripts, and the event stream are owner-scoped to that principal.
- **Boot fails closed.** The server refuses to open the shared store unless
  `OPENWAVE_AUTH_TOKENS_FILE` is set and loads cleanly — a shared database
  never comes up behind an API that cannot tell its callers apart.

The deployment posture is stated in
[decision record 4](decisions/0004-self-host-deployment-plane-authorization.md):
the server and its database run inside the operator's own network, and TLS
termination and network exposure belong to the operator's fronting
infrastructure. OpenWave serves plain HTTP and never terminates TLS.

Settings are **deployment-scoped, not per-user** — enabled providers,
credentials, model roles, and policy are shared by everyone with a token. The
profile is for mutually trusting users of one operator's deployment, not for
adversarial tenants. See the self-host section of
[how OpenWave works](how-openwave-works.md#self-host) for the full statement
and for what is still integration work.

## Prerequisites

- Docker with Compose v2.
- A machine that can reach your model provider's API.
- Somewhere private to keep the tokens file and the database password.

## Generating tokens

The token file is the credential-to-principal map. One line per token,
whitespace-separated, `#` comments and blank lines ignored:

```text
# user-id  token
alice  4f9c0e9b2d5a4c1e8f7b6a5d4c3b2a1f9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b
bob    0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
```

Generate each token with:

```sh
openssl rand -hex 32
```

Rules the loader enforces today: tokens are at least 16 characters drawn from
`[A-Za-z0-9._~-]` (32 hex bytes gives 64, comfortably over the floor); one
user may hold several tokens, which is how rotation works; a token may name
only one user, and a duplicate token fails the load.

**Roles.** Decision record 4 adds an optional third field, `admin`
(`<user-id> <token> admin`), splitting the API into a member plane and a
deployment plane and requiring at least one admin for a self-host boot to
succeed. **That role work is not implemented yet** — on this branch the loader
accepts two fields only, and every token-holder can reach the configuration
surface (including MCP server definitions, which spawn processes on the host,
and the shared provider credentials). Treat every token you hand out as an
administrator credential until the role gate lands, and give tokens only to
people you would make an administrator.

Keep the file `0600` and owned by whoever runs the stack:

```sh
umask 077
printf 'alice %s\n' "$(openssl rand -hex 32)" > deploy/self-host/tokens
```

## Environment variables

Every variable below is read by the server or the CLI; nothing here is
aspirational.

| Variable | Required | Default | What it does |
| --- | --- | --- | --- |
| `OPENWAVE_PROFILE` | yes | `desktop` | `self_host` (or `selfhost`) selects this profile. Anything else is desktop or a config error. |
| `OPENWAVE_DATABASE_URL` | yes (self-host) | — | PostgreSQL connection string for the shared store. |
| `OPENWAVE_AUTH_TOKENS_FILE` | yes (self-host) | — | Path to the token file above. Absent, boot fails before the store opens. |
| `OPENWAVE_DATA_DIR` | no | `./.openwave` | Instance lock, logs, per-turn scratch. Durable state lives in PostgreSQL, not here. |
| `OPENWAVE_LOG` | no | built-in policy | `tracing` filter directives, e.g. `debug` or `warn,openwave_server=trace`. An invalid spec falls back to the default. |
| `OPENWAVE_MODEL` | no | built-in default | Default model name; also settable at runtime through settings or per chat. |
| `OPENWAVE_MCP_CONFIG` | no | unset | External stdio MCP server configuration file loaded at boot. |
| `OPENWAVE_CONTAINER_EXECUTION_ENABLED` | no | `false` | Enables the container code-execution backend. The compose stack does not configure one. |
| `OPENWAVE_CONTAINER_IMAGE` | no | server default | Agent container image, when the above is on. |
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `XAI_API_KEY`, `GEMINI_API_KEY`, `FIREWORKS_API_KEY`, `TOGETHER_API_KEY`, `AWS_BEARER_TOKEN_BEDROCK` | no | unset | Fallback provider credentials, consulted when no credential is stored for that provider. A container has no OS keychain, so this is how a self-host deployment supplies model keys. |
| `OPENWAVE_LISTEN_PORT` | no | `8080` | Container-only: the port the image's entrypoint publishes. Not read by the server itself — see the note below. |

**The server's own listener is not configurable.** It binds `127.0.0.1` on an
ephemeral port and announces the address on stdout. The image's entrypoint
reads that announcement and bridges the fixed `OPENWAVE_LISTEN_PORT` to it, so
the container is reachable. That bridge is packaging scaffolding and should go
away once the server accepts a bind address.

## Compose quickstart

```sh
cd deploy/self-host

# 1. Tokens.
umask 077
printf 'alice %s\n' "$(openssl rand -hex 32)" > tokens

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
  `Sec-WebSocket-Protocol: openwave-token.<token>`, alongside the handshake
  subprotocol `openwave-v1`. Proxies log that header far more readily than
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

**Back up the `openwave-postgres` volume.** All durable state — chats,
projects, documents, transcripts, the event journal — lives in PostgreSQL.
The `openwave-data` volume holds only the instance lock, logs, and per-turn
scratch, and is safe to lose.

A logical dump is the simplest form:

```sh
docker compose exec -T postgres pg_dump -U openwave openwave | gzip > openwave-$(date +%F).sql.gz
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

The server applies its own schema migrations on boot. Take a database backup
before an upgrade: OpenWave is pre-1.0 and persisted formats may change
between versions (see
[decision record 2](decisions/0002-pre-v1-schema-and-persisted-format-mutability.md)).

## What is not supported yet

Self-host is a real profile with real gaps. Rather than restate them here,
read the self-host section of
[how OpenWave works](how-openwave-works.md#self-host) — it is the canonical
account. In summary, and each of these is a reason not to put irreplaceable
data in a self-host deployment yet:

- Document and blob PostgreSQL parity is not comprehensively tested (the
  durable turn state machine is what CI exercises against PostgreSQL).
- Remote secret custody is future work; this stack supplies provider keys
  through the environment, which means they are visible to anyone who can
  inspect the container.
- Object storage is not wired.
- Multi-process ownership is not wired: run exactly one server process against
  a database. The instance lock only guards one data directory, not the shared
  store.

Two more, specific to this packaging:

- The admin/member role split from decision record 4 is not implemented, so
  every token is effectively an administrator credential (see
  [Generating tokens](#generating-tokens)).
- Code execution is not configured by this stack. `exec` needs a backend, and
  none of the container backends is set up here.
