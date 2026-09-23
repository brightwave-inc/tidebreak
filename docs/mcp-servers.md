# External MCP servers

Tidebreak connects to MCP servers over three transports: a local stdio child
process, a remote Streamable HTTP endpoint, or a model-gateway MCP endpoint
bound to the signed-in gateway session. In the desktop app, open
**Settings → Connected apps → MCP servers**, add a definition, pick its
transport, and choose
**Save and verify**. For a stdio server, Tidebreak starts the executable
directly with the argument array shown in the form; it never joins the fields
into a shell command. For an HTTP server, Tidebreak sends each JSON-RPC request
as an authenticated `POST` and accepts both plain JSON and `text/event-stream`
responses.

Each server has:

- a stable namespace containing ASCII letters, numbers, `_`, or `-`;
- exactly one transport:
  - **stdio** — an executable (a bare command name or an absolute path), zero
    or more individual arguments, an optional working directory, optional
    environment variable *names* (`env`) whose values are held in the OS
    credential store, and optional `env_from` names selected from the Tidebreak
    host environment. A bare name is resolved at verify and launch time on the
    process PATH extended with the login-shell PATH the harness probe already
    captures (plus `PATHEXT` on Windows). Tidebreak never invokes a shell to
    run the server. The stored definition keeps what you typed. A relative path
    that contains separators is refused; or
  - **HTTP** — an `http`/`https` URL and an optional bearer-token variable
    name selected from the Tidebreak host environment; or
  - **gateway** — the slug of a model-gateway MCP endpoint
    (`gateway_endpoint`). The endpoint URL and a short-lived `mcp:<slug>`
    bearer are resolved from the signed-in gateway session at every
    connection; nothing is copied, selected by name, or stored. Mount these
    from **Settings → Connected apps → Gateway endpoints** with a toggle. Signed
    out, the mount degrades to a "sign in to reconnect" diagnostic and
    recovers on the next reconnect after sign-in;
- a request timeout from 1 to 3,600,000 milliseconds; and
- an enabled switch.

## Portable workspace configuration

**Settings → Git & source control** can export and import a
`.tidebreak-config.json` file (decision 83). The file is a versioned JSON
envelope of **code repository registrations** and **MCP server definitions**.

Included:

- Repositories: display name, origin URL (from `git remote get-url origin` or
  `cloned_from`), default base ref, branch prefix, setup/archive scripts,
  quick actions, and the device `root_path` as a remap hint.
- MCP servers: name, command, args, environment *names*, `env_from`, cwd,
  URL, `bearer_token_env` *name*, gateway endpoint, timeout, enabled.

Excluded: secret values (`env_values`, bearer token values, credentials),
plugin-sourced servers, connected apps, folders, plugins, providers,
preferences, transcripts, and worktrees.

On import, Tidebreak previews each entry as new, identical, or conflicting
(same repo remote or same MCP name with different fields). Device-specific
`root_path`, `command`, and `cwd` that do not exist here are marked for
remap. Each row shows what the entry runs and connects to: the command and
its arguments, the working directory, the URL, the environment names it
reads, and a repository's path, origin, and scripts. Only a new entry starts
on Add; a conflict or an entry that needs a remap starts on Skip, and Apply
never overwrites a record unless you choose Replace. A newer
`tidebreak_config` version is refused with a message to upgrade or re-export.

Apply checks every entry before it writes anything, so a refused entry
leaves this machine unchanged. Two kinds of MCP server import turned off
unless you turn on **Start after import** for them:

- A local command server. On the desktop, starting one also needs the same
  native confirmation as saving an enabled command server (decision 27): the
  app lists the commands in an OS dialog, and nothing is imported if you
  decline. `POST /workspace-config/apply` refuses an import that would start
  a local command on the desktop with `native_confirmation_required`.
- A remote server that sends a credential from this computer's environment,
  which today means one with a bearer token variable. Its row says which
  variable it sends to which host, for example "Sends GITHUB_TOKEN to
  mcp.example.com." The switch is the consent; no OS dialog follows. Without
  an explicit choice, apply writes such a server turned off even when the
  file has it on.

A remote server that sends nothing from this computer imports as the file has
it. On a multi-user deployment, only an administrator can import MCP servers,
the same rule `PUT /mcp/servers` follows; a member can still import
repository entries.

The child environment starts empty, and **no environment value of any kind
lives in a definition**. Executables, arguments, working directories, and URLs
are ordinary displayed settings, so do not put credentials in any of those
fields. The two channels that do carry a value are:

- **Environment** (`env`) — the definition holds the variable names; the values
  live in the OS credential store, keyed by the server's connected-app record.
  Settings shows a password field per name that starts blank and keeps the
  stored value if you leave it blank. The values are never returned to the
  renderer and never enter SQLite. Deleting a name, or the server, deletes its
  stored value.
- **`env_from`** and **Bearer token variable** — a name selected from the
  environment that launched Tidebreak, resolved at the connection boundary and
  never stored at all.

A missing selected name produces a server-specific error containing the name,
not a value, and tells you to export it in the shell you start Tidebreak from
and then restart Tidebreak. Save and verify classifies the failure: DNS
resolution (the host), TLS handshake (the reason), HTTP status (401/403 as
authentication, 404 as wrong path, 5xx as server error, with the status line),
protocol negotiation (quotes the first bytes when the endpoint answered but
not with MCP JSON-RPC), timeout (after N ms), or a stdio failure: command not
found (names a bounded list of directories searched), not executable or
permission denied, launch failure before the MCP handshake (exit status and a
bounded stderr tail), or MCP protocol failure. A successful stdio verify
reports the resolved executable path. A `401` from a remote server that asks
for an OAuth sign-in is not a failure; see
[OAuth for remote HTTP servers](#oauth-for-remote-http-servers). Diagnostics
never echo a URL, token, argument value, or environment value. Child stderr is
not copied into host logs; a failed launch may quote its first line in
Settings.

Definitions saved before the values moved into the credential store held them
in cleartext in the connected-app record. They are migrated on first load: the
values move to the credential store and the record is rewritten with names
only. Names are all the definition fingerprint ever covered, so existing app
grants stay valid across the migration.

All mounted names use `mcp__{namespace}__{remote_tool}`. MCP tools are sensitive:
the existing Tidebreak approval gate must approve each call before it crosses
the process boundary. MCP approvals cannot be remembered for the chat. A server
definition can change behind a stable namespace, so reusing a name-based grant
would silently widen its authority.

## Tested and community servers

Each configured server carries one of two labels in Settings: **Tested** when
it matches Tidebreak's curated list of servers we have exercised end to end, and
**Community** otherwise. The label gates nothing — both tiers mount, connect,
and call identically. See
[Tested and community MCP servers](mcp-tested-servers.md) for what the tested
claim covers and how a server earns an entry.

## Directory

**Settings → Connected apps → MCP servers** opens with a directory of
well-known remote MCP servers. Each entry names the server, what it lets you
do, how it signs in, and the host it connects to. Search matches the name, the
description, and the host.

**Add** saves the entry as an ordinary remote HTTP server and connects only
that server, so a configured server that is down cannot block the add. The
server is saved even when it cannot connect yet, and its row says what it
needs:

- **Sign in with your browser** — the server answers `401` and asks for an
  OAuth sign-in. Tidebreak starts the
  [sign-in](#oauth-for-remote-http-servers) right after the add and opens the
  page in your browser. In a window attached to another machine, the row
  offers Connect instead, because the sign-in has to finish on that machine.
- **Sends `VARIABLE` to `host`** — the server takes a token as a bearer, and
  its first connect sends the value of that environment variable to the
  vendor's host. The row asks first, with a **Start after adding** switch
  that starts off, the same switch an import shows before it connects a
  server that sends a credential. Left off, Add saves the server turned off,
  and it sends nothing until you turn it on in Connected apps. Tidebreak
  reads the variable from the environment it started with, so export it in
  the shell you start Tidebreak from.
- **No sign-in** — the server is public and connects at once.

An add fails, and saves nothing, when the server asks for an OAuth sign-in
Tidebreak can never complete, or when Tidebreak already holds its limit of
servers. Adding a server whose address is already configured changes nothing,
and Settings shows the entry as **Added**. On a managed profile the directory
is hidden, and `POST /mcp/directory/{id}/add` refuses the add like any other
remote server a person types in. The route's body is `{"start": true}` or
`{"start": false}`, and it has no default: a client says whether the server
connects.

Each entry is an endpoint its vendor hosts and publishes in its own
documentation. Tidebreak holds no OAuth app for any of them: a server that
signs in registers Tidebreak with its own sign-in service each time (RFC
7591), the same as any remote server that asks. The directory claims no tier.
An entry shows **Tested** only when the curated list vouches for its address.
A vendor that hosts its server in more than one region gets one entry per
region: Intercom has a US entry and an EU entry. Intercom does not host its
server for Australian workspaces yet.

The entries live in `crates/tidebreak-server/src/mcp_directory.json`, one
server per line, compiled into the build. To add or correct an entry, change
its line: `id` is the name the added server gets, `url` is the endpoint exactly
as the vendor's documentation prints it, `docs` is that page, and `sign_in` is
`{"kind": "oauth"}`, `{"kind": "token", "variable": "NAME"}`, or
`{"kind": "none"}`. The unit tests check that the file parses, that every entry
uses `https`, and that it makes a valid server definition.

## OAuth for remote HTTP servers

A remote server that asks you to sign in shows **Sign in required** and a
**Connect** button in Settings, with the host of the sign-in page beside it,
for example "Opens vercel.com", so you see where Connect sends you. You do not
have to mark the server as OAuth. When an HTTP server that sends no bearer
token answers `401`, Tidebreak asks it how to authorize, and a server whose
metadata names an authorization server gets a sign-in. Imported definitions
and definitions saved before this behave the same way. The saved `oauth` flag
still forces the OAuth path. You never paste a token into the definition.

**Save and verify** keeps a server that asks you to sign in, because you can
connect only a saved server. A server whose sign-in Tidebreak cannot complete
fails the save and says why.

The flow follows the MCP authorization specification:

1. A `401` `WWW-Authenticate` challenge may name protected-resource metadata
   (RFC 9728). If it does not, or that document does not load, Tidebreak tries
   `/.well-known/oauth-protected-resource` with the server's path after it,
   then at the origin root. The document's `resource` must be the server's URL
   or a parent path on the same origin (RFC 9728 §3.3); other metadata is not
   used.
2. That document names authorization servers. Tidebreak takes the first and
   reads its RFC 8414 metadata, or its OpenID Connect discovery document. The
   metadata's `issuer` must be the authorization server it was fetched for
   (RFC 8414 §3.3), and when it lists PKCE methods, S256 must be one of them.
3. The authorization server must advertise a dynamic-registration endpoint
   (RFC 7591). Without one, the server is **Unsupported** — a desktop install
   has no pre-issued client id.
4. When you select Connect, Tidebreak registers a public client
   (`token_endpoint_auth_method: none`, no client secret) for an ephemeral
   loopback redirect, and answers with an authorization-code + PKCE S256 page
   (RFC 8252 §7.3). The desktop opens that page in your browser; the server
   never opens a browser itself. The request asks for the scope the challenge
   named, or else the protected resource's `scopes_supported`, and carries the
   RFC 8707 `resource` indicator for the server.
5. After you approve, the loopback listener exchanges the code and stores the
   tokens and the client registration, bound to the server's exact URL. The
   server reconnects, and the row reads **Connecting** until its tools load.
   Later calls present a refreshing access token as the per-call bearer.

While the sign-in waits, Settings shows **Waiting for sign-in** with **Reopen
sign-in page** and **Cancel**, and it updates when you finish. Cancel stops the
sign-in and leaves any stored session alone. The sign-in gives up after five
minutes. The loopback redirect goes to the computer that runs Tidebreak's
server, so the sign-in has to finish in a browser on that computer. A window
attached to another machine says so instead of offering to try again.

Connection states:

- **Unsupported** — the server asks for OAuth in a way Tidebreak cannot
  complete: no dynamic client registration, metadata that describes another
  server or names another issuer, no S256, or an authorization server that
  publishes no readable metadata or is not at a public `https` address. The row
  says which.
- **Not connected** — the server asks you to sign in and no session is stored;
  Connect. After a sign-in that failed, the row says what stopped it, such as a
  timeout or a server that refused to register Tidebreak. When the sign-in
  service does not answer, the row says so, and Tidebreak keeps retrying.
- **Authorizing** — a browser authorization is in flight.
- **Connected** — a usable session is stored (a fresh access token, or a stale
  access token that still has a refresh token).
- **Expired** — the stored session expired with no refresh token, or the server
  no longer accepts it.
- **Access denied** — you declined on the sign-in page, or the authorization
  server refused you.

A session ends only when the sign-in service refuses its refresh token. A
refresh the service does not answer — a timeout, a network failure, or a `5xx`
— keeps the session, and the server retries on its usual backoff. So does a
sign-in service that does not answer while Tidebreak discovers how to sign in.

Security posture:

- Access tokens, refresh tokens, and the registered client record live only in
  the OS credential store, keyed by the connected-app id. They never enter
  SQLite, the definition, logs, error strings, arguments, or API responses.
- A session is bound to the exact server URL it was issued for and is
  presented to no other. Editing a server's URL, or removing the server,
  clears its session, so a server at a new address, or a later server with the
  same name, never inherits it.
- Every discovery, registration, token, and refresh fetch is admitted before
  the request is sent. Server-controlled metadata (`resource_metadata`, each
  `authorization_servers` entry) gets the same check. There is no loopback
  exception: an OAuth token never leaves for a private or loopback address
  named by the server. The client resolves each name through the same check
  when it connects, so the address it reaches is one that passed.
- Discovery, registration, and token responses larger than 256 KiB are not
  read, and every OAuth request stops after 30 seconds.
- The HTTP client refuses redirects. Diagnostics never echo a URL, token,
  argument value, or environment value.

Disconnect clears the stored session and registration and drops the live
connection. Status is a read: it never mutates tokens.

## Health and refresh

Settings reports `initializing`, `healthy`, `degraded`, `reconnecting`, or
`disabled` plus a bounded diagnostic and tool count. A healthy verify names
how many tools were discovered and that the server is available to new turns.
A disabled server stays configured and is not available to new turns. The
runtime periodically pings enabled servers with a fixed health deadline and
retries unavailable sessions with capped exponential backoff. Health checks and reconnects run
independently across servers, while duplicate reconnects for one server share a
single attempt. A server busy with a tool call is skipped for that health cycle,
not treated as degraded. **Reconnect and refresh tools** explicitly starts a
fresh session and rediscovers its tool list. The runtime does the same after the
server emits `notifications/tools/list_changed`.

When Tidebreak starts, it opens its port first and connects the saved servers
in the background, all at once. Each saved server reads **Connecting** until
its first connection finishes, and publishes its tools the moment it is up, so
one slow server holds up neither the app nor the servers beside it. Settings
reads the list again while a server is connecting.

Work that needs the tool list waits for the saved servers against one
deadline: three seconds after Tidebreak published them. Once the deadline has
passed, nothing waits, however long a server keeps connecting. A turn on
Tidebreak's own engine that starts before every server is up runs with the
servers that are up, and its system prompt names the apps still connecting,
so the model says an app is still connecting instead of saying it is not
connected. Their tools join the next turn. An external engine reaches the
same tools through the connected-apps bridge
(`/code/mcp/connected-apps`). There, only a tool list waits for the deadline;
every other request answers at once. A list built while servers connect
carries a `connected_apps_status` tool whose description names them, and
calling it reports which apps are up. The bridge declares
`tools.listChanged`, and its event stream (`GET` on the same path) sends
`notifications/tools/list_changed` whenever the tools an engine last listed
are out of date, so an engine that follows it lists again when a server
comes up, reconnects with other tools, or goes away.

A saved record Tidebreak cannot load does not stop it from starting. That is a
record whose definition does not decode, for example one a newer version wrote
with a setting this version does not know; one that fails validation; or one
from before environment values moved into the credential store whose values
cannot move now. Tidebreak skips the record, and **Connected apps** lists it
with its name and why, and a **Remove** action that deletes it with the values
and sign-in stored under it. A skipped record's tools never mount. A save keeps
the record on file, unchanged, and its name stays taken until you remove it.

Three failures stop the automatic retries, because retrying cannot fix them. A
gateway mount without a gateway session waits for the next sign-in. A server
whose parent environment variable is missing waits for a settings change or a
manual reconnect. A remote server that asks for an OAuth sign-in waits for you
to connect it, a settings change, or a manual reconnect; one whose sign-in
service did not answer keeps retrying. Each way the server stays `degraded`
with its diagnostic, and **Reconnect and refresh tools** still tries at once.
The log records a reconnect failure when it first happens or changes, not on
every retry.

Saving a candidate connects every enabled server before replacing the current
set. If validation or initialization fails, the previous set remains active. A
remote server that asks you to sign in does not fail the save: it is saved and
waits for Connect. Adding a server from the [directory](#directory) is the
exception: it connects only the new server. Each running turn holds an
immutable registry snapshot, so a configuration or tool-list change applies
only to subsequent turns.

Discovery is fail-closed and bounded. Mounted names must fit the provider-safe
64-byte `[A-Za-z0-9_-]` contract after namespacing. Tidebreak caps JSON-RPC frame
size, tool count, pagination, cursors, descriptions, individual schemas, and
aggregate tool metadata before publishing a connection. A server that exceeds a
limit stays out of the active tool set and receives only a fixed diagnostic in
Settings.

## MCP App views

A server may declare an [MCP Apps](https://github.com/modelcontextprotocol/ext-apps)
view for a tool through `_meta` (`ui.resourceUri`, or the legacy flat
`ui/resourceUri` spelling). Tidebreak validates the declaration at discovery —
it must be a bounded, control-character-free `ui://` URI; a malformed
declaration fails the connection — and prefetches the document once per
connection through `resources/read`, bounded at 1 MiB. A server's views are
fetched at the same time, each within five seconds, and its tools publish once
they arrive. A view that does not arrive in time is left out, like one that
fails.

When such a tool completes successfully, its transcript card renders the
declared view. The renderer event stream itself carries only a typed
reference (the configured server namespace and the validated URI). The
renderer never holds the markup at all: it trades its bearer for a
single-use, minute-lived frame token, and the iframe loads the document from
the host, which serves it with its own strict Content-Security-Policy — an
http-served frame does not inherit the app's policy the way a `blob:` or
`srcdoc` document would, so the view's inline script runs while its network
egress stays shut. The frame is sandboxed with `allow-scripts` only and is
never same-origin with the app: it has no access to Tidebreak's DOM, storage,
bearer token, or IPC surface. Remote tool names, descriptions, and raw tool
output still never reach the renderer.

Views are served from memory and refreshed on reconnect. If a view cannot be
fetched, its card degrades to a reconnect hint; the tool itself is unaffected.

The view surface is deliberately frozen at this scope. The bridge answers
`ui/initialize` with empty host capabilities and refuses every other request:
a view renders one call's delivered payload and never initiates calls, which
is why it is safe to run with no consent surface of its own. Any future
view-initiated interactivity must ride the local-app grant machinery
([local-apps.md](local-apps.md)) rather than a new approval door. Revisit
point on record: once local apps and gateway promotion have shipped, if the
gateway's inline console remains the only `ui://` producer in practice and a
promoted app covers its use case, deprecating this surface is the recorded
default — it would remove a special case, not a subsystem.

## Known-good setups

Fill the Connected apps form exactly as below, then **Save and verify**. A
healthy row reports the discovered tool count and that the server is available
to new turns.

### Stdio: Tidebreak workspace tools

Tidebreak's own read-only workspace server. The executable is the `tidebreak`
binary on your `PATH`, or its absolute path.

- **Namespace:** `workspace` (ASCII letters, numbers, `_`, or `-`)
- **Transport:** Process on this computer (stdio)
- **Executable:** `tidebreak`
- **Arguments:** `mcp`, then the absolute workspace path (`/absolute/path/to/workspace`)
- **Working directory:** leave blank
- **Environment / Forward environment names:** none
- **Bearer token variable:** not used
- **Request timeout:** `60000`
- **Enabled:** on

### Remote HTTP: loopback Streamable HTTP

A server that speaks MCP Streamable HTTP on loopback. Use `http://` only for a
literal loopback address; remote URLs that send a bearer token must use
`https://`.

- **Namespace:** `docs`
- **Transport:** Remote endpoint (HTTP)
- **Server URL:** `http://127.0.0.1:8080/mcp` (the path your server actually serves)
- **Bearer token variable:** leave blank on loopback with no auth
- **Request timeout:** `60000`
- **Enabled:** on

For a remote HTTPS server that expects a bearer token, set **Server URL** to
the `https://` MCP path and **Bearer token variable** to the *name* of the
variable (for example `GATEWAY_TOKEN`). Export that variable in the shell you
start Tidebreak from, then restart Tidebreak. Tidebreak reads its process
environment; it does not read a `.env` file or the token from this form. A
Dock or Finder launch does not see variables you set only in another terminal.

## Headless bootstrap

`tidebreak serve` can still read an initial configuration from the JSON file
named by `TIDEBREAK_MCP_CONFIG`:

```json
{
  "servers": [
    {
      "name": "documents",
      "command": "/absolute/path/to/documents-mcp",
      "args": ["--stdio"],
      "cwd": "/absolute/path/to/workspace",
      "env": ["LOG_LEVEL"],
      "env_values": {
        "LOG_LEVEL": "info"
      },
      "env_from": ["DOCUMENTS_TOKEN"],
      "request_timeout_ms": 60000,
      "enabled": true
    },
    {
      "name": "gateway",
      "url": "https://gateway.example/mcp/tools",
      "bearer_token_env": "GATEWAY_TOKEN",
      "request_timeout_ms": 60000,
      "enabled": true
    }
  ]
}
```

The schema is closed, including at the API boundary. Broad process-environment
inheritance is not supported. `env_values` is an input only — it is written to
the credential store and never appears in a response or a saved record, and a
bootstrap file's values land in the same place as any other. When there is no
saved desktop configuration, a malformed bootstrap file, missing selected
environment name, or failed enabled server makes startup fail rather than
silently narrowing the advertised tools. That check needs the file's servers
connected, so unlike saved servers they connect before the port opens.
