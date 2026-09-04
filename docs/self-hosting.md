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

Selecting `TIDEBREAK_PROFILE=self_host` changes five things about the server:

- **The store is PostgreSQL**, opened from `TIDEBREAK_DATABASE_URL`, and the
  binary must be built with tidebreak-server's `postgres` feature for the
  driver to exist at all.
- **Every request must name a user.** The desktop profile's per-launch bearer
  token authenticates nobody here. A hosted deployment validates short-lived
  Model Gateway `tidebreak` resource tokens; a standalone deployment can use
  the operator-managed token file. Both resolve to a named principal carrying
  a role. Chats, projects, documents, transcripts, code workspaces, and event
  streams are owner-scoped to that principal.
- **Blob bytes live in S3-compatible object storage**, selected by
  `TIDEBREAK_BLOB_STORE_URL`. PostgreSQL keeps the document catalog and
  references; the bucket keeps immutable source bytes, images, and artifacts.
- **Boot fails closed.** The server refuses to open the shared store unless
  exactly one of `TIDEBREAK_AUTH_GATEWAY_URL` or
  `TIDEBREAK_AUTH_TOKENS_FILE` is valid — a shared database never comes up
  behind an API that cannot tell its callers apart.
- **Stored credentials use Vault KV v2.** The server never opens the desktop
  OS keychain. When Vault is not configured, provider environment variables
  remain available as read fallbacks, but deployment-plane credential writes
  and deletes fail with setup guidance.

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

## Prerequisites

- Docker with Compose v2.
- A machine that can reach your model provider's API.
- Somewhere private to keep the database password, plus either a Model Gateway
  installation or a standalone tokens file.
- A Vault KV v2 mount if administrators need to save shared credentials through
  Tidebreak. Provider environment variables remain available without Vault.

## Model Gateway identity (hosted default)

Set `TIDEBREAK_AUTH_GATEWAY_URL` to the Model Gateway base URL. Tidebreak
desktop's “Connect with Model Gateway” flow discovers this URL from the hosted
machine, mints a short-lived `tidebreak` resource token from the OAuth session
the app already holds, and refreshes it automatically.

The hosted server asks the Gateway to resolve that token on every request.
Active Gateway users become Tidebreak members, Gateway administrators become
Tidebreak administrators, and the stable Gateway user UUID becomes the owner
key. Deactivation, session revocation, and role changes require no Tidebreak
roster update or token redistribution. If the Gateway cannot validate a token,
the request is refused.

Do not set `TIDEBREAK_AUTH_TOKENS_FILE` in this mode. Selecting both mechanisms
is an ambiguous configuration and the server refuses to start.

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

## Generating tokens for standalone compatibility

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
[decision record 6](decisions/0006-self-host-deployment-plane-authorization.md).

### What a member runs

For the standalone token-file mode, the member surface is the HTTP API and the
`tidebreak` CLI pointed at the deployment. Give each teammate a token from the
file above and a base URL:

```sh
export TIDEBREAK_SERVER_URL=https://tidebreak.example
export TIDEBREAK_SERVER_TOKEN=<the member token>
cargo run -p tidebreak-cli -- --server "$TIDEBREAK_SERVER_URL" chat list
cargo run -p tidebreak-cli -- --server "$TIDEBREAK_SERVER_URL" -p "summarize yesterday"
```

`--server` / `TIDEBREAK_SERVER_TOKEN` are the same attach path the headless
docs describe. A member token receives `403` on deployment-plane routes, which
is the intended degradation — not a desktop Settings panel. Remote server URLs
must use HTTPS; cleartext HTTP is available only for loopback development.

The packaged desktop app still embeds its local Desktop-profile server, but it
can attach its renderer to a remote self-host machine. For a Gateway-backed
machine, Settings → Model Gateway → “Connect with Model Gateway” reuses the
app's managed Gateway session and stores no Tidebreak user token. The address
is filled in for you when the Gateway names the machine it hosts. “Connect with
token”, under Advanced, remains available for this standalone compatibility
mode.

Give `admin` only to the people who actually administer the deployment: MCP
server definitions spawn processes on the host, and the provider credentials
are shared.

Keep the file `0600` and owned by whoever runs the stack:

```sh
umask 077
printf 'alice %s admin\n' "$(openssl rand -hex 32)" > deploy/self-host/tokens
```

## Vault credential custody

To save provider, web-search, code-execution, and connected-app credentials
through Tidebreak, give the self-host server a HashiCorp Vault KV v2 mount.
The server stores no Vault token in its database or boot configuration. It
reads the token from a mounted file for every Vault request, so an injector or
Vault Agent can rotate the file without restarting Tidebreak.

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

If Vault is absent, stored-secret reads return unset so provider environment
variables keep working. Attempts to save or remove a credential fail and name
`TIDEBREAK_VAULT_ADDR` and `TIDEBREAK_VAULT_TOKEN_FILE` as the required setup.

## Environment variables

Every variable below is read by the server or the CLI; nothing here is
aspirational.

| Variable | Required | Default | What it does |
| --- | --- | --- | --- |
| `TIDEBREAK_PROFILE` | yes | `desktop` | `self_host` (or `selfhost`) selects this profile. Anything else is desktop or a config error. |
| `TIDEBREAK_DATABASE_URL` | yes (self-host) | `DATABASE_URL` | PostgreSQL connection string for the shared store. On a Model Gateway managed machine the plane's `DATABASE_URL` stands in when this is unset. |
| `TIDEBREAK_BLOB_STORE_URL` | yes (self-host) | — | S3 bucket and optional prefix, for example `s3://company-tidebreak/production`. Credentials, region, and an optional compatible endpoint come from standard `AWS_*` variables. |
| `AWS_DEFAULT_REGION`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_ENDPOINT_URL_S3`, `AWS_ALLOW_HTTP` | depends on provider | AWS defaults | Configure AWS S3 or an S3-compatible endpoint. Keep `AWS_ALLOW_HTTP=false` outside isolated development networks. Role, web-identity, and container credential variables are also accepted. |
| `TIDEBREAK_AUTH_GATEWAY_URL` | one auth mode required | `GATEWAY_BASE_URL` | Public Model Gateway identity URL exposed to clients and, by default, used for live validation. HTTPS required except for loopback development. |
| `TIDEBREAK_PUBLIC_URL` | with Gateway auth | `ADD_ON_PUBLIC_URL` | The machine's own public URL, which user credentials are bound to. On a Model Gateway managed machine the plane's `ADD_ON_PUBLIC_URL` stands in when this is unset (decision 0085). |
| `TIDEBREAK_AUTH_GATEWAY_VERIFIER_URL` | no | `TIDEBREAK_AUTH_GATEWAY_URL` | Optional server-to-server Gateway URL for principal validation when the public origin is not cluster-routable. Requires Gateway auth. |
| `TIDEBREAK_AUTH_TOKENS_FILE` | one auth mode required | — | Standalone compatibility: path to the static token file above. Mutually exclusive with Gateway auth. |
| `TIDEBREAK_ADAPTER_BOOTSTRAP_TOKENS` | no | unset | Comma-separated service bearers allowed to start an external channel connect handshake. Each value must be 32–512 header-safe characters. Leave unset to disable connect start. To rotate without downtime, add the new value, move the adapter, then remove the old value. |
| `TIDEBREAK_VAULT_ADDR` | required with Vault custody | — | Vault base URL. HTTPS is required except for literal loopback development. Setting any Vault option enables Vault configuration and requires this variable plus `TIDEBREAK_VAULT_TOKEN_FILE`. |
| `TIDEBREAK_VAULT_TOKEN_FILE` | required with Vault custody | — | Mounted file containing the Vault token. Tidebreak reads it for every request so rotation does not require a restart. |
| `TIDEBREAK_VAULT_MOUNT` | no | `secret` | KV v2 mount path. |
| `TIDEBREAK_VAULT_PATH` | no | `tidebreak` | Deployment-specific path below the mount. Tidebreak appends one encoded credential key. |
| `TIDEBREAK_VAULT_NAMESPACE` | no | unset | Vault Enterprise or HCP namespace sent as `X-Vault-Namespace`. |
| `TIDEBREAK_DATA_DIR` | no | `./.tidebreak` | Instance lock, logs, per-turn scratch. Durable state lives in PostgreSQL, not here. |
| `HOME` | no | `/var/lib/tidebreak/home` in the image | Writable home for npm and the coding harnesses. The image keeps it on the data volume because a hosting plane may run the container as a uid with no passwd entry, which is otherwise handed `HOME=/`. The server creates it at boot. |
| `TIDEBREAK_LOG` | no | built-in policy | `tracing` filter directives, e.g. `debug` or `warn,tidebreak_server=trace`. An invalid spec falls back to the default. |
| `TIDEBREAK_DIAGNOSTICS_LOG` | no | `off,tidebreak_diagnostics=info` | `tracing` filter directives for the bounded structured JSONL log. See [Diagnostics](diagnostics.md). |
| `TIDEBREAK_MODEL` | no | built-in default | Default model name; also settable at runtime through settings or per chat. |
| `TIDEBREAK_MCP_CONFIG` | no | unset | External stdio MCP server configuration file loaded at boot. |
| `TIDEBREAK_CONTAINER_EXECUTION_ENABLED` | no | `false` | Enables the container code-execution backend. The compose stack does not configure one. |
| `TIDEBREAK_CONTAINER_IMAGE` | no | server default | Agent container image, when the above is on. |
| `TIDEBREAK_RUNTIME_ENDPOINT` | remote sessions | unset | Model Gateway runtime endpoint slug used to provision remote code sessions. Requires Gateway authentication and `TIDEBREAK_RUNTIME_PROFILE`. |
| `TIDEBREAK_RUNTIME_PROFILE` | remote sessions | unset | Administrator-defined sandbox profile sent with every remote spawn. Requires `TIDEBREAK_RUNTIME_ENDPOINT`. |
| `TIDEBREAK_RUNTIME_CONCURRENCY_CAP` | no | `3` | Positive maximum number of live remote sandboxes each owner may hold. Restart Tidebreak after changing it. |
| `TIDEBREAK_RUNTIME_SPAWN_SPEND_CEILING_MICROUSD` | no | `5000000` | Positive per-spawn spend ceiling in micro-USD. Set `none` to leave this ceiling to the runtime profile. Restart Tidebreak after changing it. |
| `TIDEBREAK_RUNTIME_SESSION_SPEND_CEILING_MICROUSD` | no | `20000000` | Positive cumulative spend ceiling per remote session in micro-USD. Set `none` to remove Tidebreak's cumulative ceiling; the runtime profile still bounds each spawn. Restart Tidebreak after changing it. |
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `XAI_API_KEY`, `GEMINI_API_KEY`, `FIREWORKS_API_KEY`, `TOGETHER_API_KEY` | no | unset | Fallback provider credentials, consulted when Vault holds no credential for that provider or Vault custody is not configured. |
| `ANTHROPIC_BASE_URL`, `OPENAI_BASE_URL`, `OPENAI_COMPATIBLE_BASE_URL`, `OLLAMA_BASE_URL` | no | unset | Fallback provider endpoints, consulted when no base URL is stored for that provider. Point a provider at a compatible endpoint from your chart or compose file instead of setting it after first boot. Use HTTPS; Ollama also accepts HTTP on a loopback address. An unusable value is ignored, and the provider keeps its built-in endpoint. |
| `TIDEBREAK_LISTEN_ADDR` | no | loopback, ephemeral port | Self-host only: the address and port the API binds, e.g. `0.0.0.0:8080`. The desktop profile refuses to boot with it set — that profile's loopback binding is what its per-launch token assumes. The image sets it to `0.0.0.0:8080` so the container is reachable at a known port. |
| `TIDEBREAK_UI_DIST` | no | unset | A built desktop renderer bundle to serve to browsers; see [Opening the machine in a browser](#opening-the-machine-in-a-browser). The image sets it to the bundle it carries. Unset, the server serves no pages and an unknown path answers `404`. The server refuses to start if the directory holds no `index.html`. |

## Compose quickstart

```sh
cd deploy/self-host

# 1. Tokens. At least one line must be an admin, or the server will not boot.
umask 077
printf 'alice %s admin\n' "$(openssl rand -hex 32)" > tokens

# 2. Database password, object storage, and provider key.
cat > .env <<'EOF'
POSTGRES_PASSWORD=<a long random string>
TIDEBREAK_BLOB_STORE_URL=s3://company-tidebreak/production
AWS_DEFAULT_REGION=us-east-1
AWS_ACCESS_KEY_ID=<your access key>
AWS_SECRET_ACCESS_KEY=<your secret key>
ANTHROPIC_API_KEY=<your key>
EOF
chmod 600 .env

# 3. Build and start. The first build compiles the workspace and is slow.
docker compose up -d --build

# 4. Confirm it is up.
curl -fsS http://127.0.0.1:8080/healthz     # -> ok
```

This minimal Compose stack uses provider environment variables. To save
credentials through Settings, mount a Vault token file into the server
container and pass the `TIDEBREAK_VAULT_*` variables from the preceding
section.

`/healthz` is the one unauthenticated route. Everything else needs
`Authorization: Bearer <token>` with a token from your file.

The stack publishes `127.0.0.1:8080` deliberately. Nothing in
`docker-compose.yml` terminates TLS, and the database port is not published
at all.

## What the image provides

Code mode runs agents on the machine, so the image carries the tools it
spawns. Read this before you swap in a base image of your own: the server
does not install any of it, and reports a missing piece as an unavailable
engine rather than an installation prompt.

| Tool | Version | Why it is there |
| --- | --- | --- |
| Managed Node runtime | 20.20.2, at `/opt/tidebreak/node/20.20.2` | The runtime every harness install runs `npm` from. |
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

- **A GitHub identity.** Tidebreak observes `gh`'s authentication and never
  reads or stores a token ([decision record
  34](decisions/0034-harness-discovery-credentials.md)), and a container has
  no terminal to run `gh auth login` in. Set `GH_TOKEN` in the server's
  environment and `gh` picks it up. Everyone on the deployment then acts as
  that one account; per-user GitHub identity is not built yet.
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

A tab signs in the way the desktop does: with a short-lived Gateway bearer
bound to this machine. The page holds that bearer in memory for the tab's
life and never in a cookie or in storage. It arrives once, from the Model
Gateway console's Manage action: the console sends the browser to the
machine's `/auth/handoff` route with a one-time code, the machine exchanges
the code with the gateway server to server, and the page receives the bearer
in the URL fragment, which no server or access log sees. The bearer lasts
its hour; a tab that opens the address directly, outlives its bearer, or
arrives with a code that has already been used, shows a sign-in screen that
sends the reader back through the console. A machine on static tokens
(`TIDEBREAK_AUTH_TOKENS_FILE`) has no browser sign-in: the page says so, and
the desktop app remains the client for it.

Everything that reaches the reader's own computer — connected folders, tool
calls on the local machine, saving files locally, computer use — is
unavailable in a browser tab, as it is for any remote attachment.

To run the image without pages, unset `TIDEBREAK_UI_DIST`.

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

**Back up PostgreSQL and the object-store prefix.** PostgreSQL holds chats,
projects, document records, transcripts, and the event journal. The bucket
holds the immutable blob bytes those records reference. Restore both from the
same backup window.

The `tidebreak-data` volume holds only the instance lock, logs, and per-turn
scratch, and is safe to lose.

A logical dump is the simplest form:

```sh
docker compose exec -T postgres pg_dump -U tidebreak tidebreak | gzip > tidebreak-$(date +%F).sql.gz
```

Restore into a fresh, empty database before starting the server against it.

The `.env` file is not in either volume. Back it up separately as a secret. In
standalone compatibility mode, back up the tokens file too; Gateway-backed
mode has no Tidebreak token file.

Grant `s3:ListBucket` for the configured prefix. Grant `s3:GetObject`,
`s3:PutObject`, `s3:DeleteObject`, and `s3:AbortMultipartUpload` only for
objects below that prefix. Configure the bucket to abort incomplete multipart
uploads after a day. Also expire completed objects in the `_uploads/` path
below that prefix after a day because streamed writes publish through that
temporary path.

## Upgrading

```sh
git pull
docker compose up -d --build
docker image prune -f     # optional
```

The runtime image pins both Debian base images by digest and installs its
runtime package set from a dated Debian snapshot with exact direct versions.
The two artifacts it fetches from outside Debian — the managed Node runtime
and `gh` — are pinned by version and SHA-256 and verified before they are
unpacked. That keeps a rebuild of one commit from silently picking up
different `curl`, CA-certificate, Node, `gh`, or transitive package bytes.
Updating the snapshot date and the pins is therefore an explicit
dependency-maintenance change rather than an incidental effect of rebuilding.

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

- Tidebreak enforces one active server process per PostgreSQL database through
  a dedicated advisory lease. A second process refuses boot even when it uses
  another data directory. Horizontal multi-process serving remains unsupported
  until every process-local worker and live-delivery path has a distributed
  owner.

One more, specific to this packaging:

- Code execution is not configured by this stack. `exec` needs a backend, and
  none of the container backends is set up here.
