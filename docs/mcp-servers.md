# External MCP servers

OpenWave connects to MCP servers over three transports: a local stdio child
process, a remote Streamable HTTP endpoint, or a model-gateway MCP endpoint
bound to the signed-in gateway session. In the desktop app, open
**Settings → Connected apps → MCP servers**, add a definition, pick its
transport, and choose
**Save and verify**. For a stdio server, OpenWave starts the executable
directly with the argument array shown in the form; it never joins the fields
into a shell command. For an HTTP server, OpenWave sends each JSON-RPC request
as an authenticated `POST` and accepts both plain JSON and `text/event-stream`
responses.

Each server has:

- a stable namespace containing ASCII letters, numbers, `_`, or `-`;
- exactly one transport:
  - **stdio** — an executable, zero or more individual arguments, an optional
    working directory, optional environment variable *names* (`env`) whose
    values are held in the OS credential store, and optional `env_from` names
    selected from the OpenWave host environment; or
  - **HTTP** — an `http`/`https` URL and an optional bearer-token variable
    name selected from the OpenWave host environment; or
  - **gateway** — the slug of a model-gateway MCP endpoint
    (`gateway_endpoint`). The endpoint URL and a short-lived `mcp:<slug>`
    bearer are resolved from the signed-in gateway session at every
    connection; nothing is copied, selected by name, or stored. Mount these
    from **Settings → Connected apps → Gateway endpoints** with a toggle. Signed
    out, the mount degrades to a "sign in to reconnect" diagnostic and
    recovers on the next reconnect after sign-in;
- a request timeout from 1 to 3,600,000 milliseconds; and
- an enabled switch.

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
  environment that launched OpenWave, resolved at the connection boundary and
  never stored at all.

A missing selected name produces a server-specific error containing the name,
not a value. Child stderr is discarded so a server cannot copy a forwarded
credential into OpenWave's host logs, and HTTP diagnostics are fixed strings
that never echo the URL, a token, or a response body.

Definitions saved before the values moved into the credential store held them
in cleartext in the connected-app record. They are migrated on first load: the
values move to the credential store and the record is rewritten with names
only. Names are all the definition fingerprint ever covered, so existing app
grants stay valid across the migration.

All mounted names use `mcp__{namespace}__{remote_tool}`. MCP tools are sensitive:
the existing OpenWave approval gate must approve each call before it crosses
the process boundary. MCP approvals cannot be remembered for the chat. A server
definition can change behind a stable namespace, so reusing a name-based grant
would silently widen its authority.

## Tested and community servers

Each configured server carries one of two labels in Settings: **Tested** when
it matches OpenWave's curated list of servers we have exercised end to end, and
**Community** otherwise. The label gates nothing — both tiers mount, connect,
and call identically. See
[Tested and community MCP servers](mcp-tested-servers.md) for what the tested
claim covers and how a server earns an entry.

## Health and refresh

Settings reports `initializing`, `healthy`, `degraded`, `reconnecting`, or
`disabled` plus a bounded diagnostic and tool count. The runtime periodically
pings enabled servers with a fixed health deadline and retries unavailable
sessions with capped exponential backoff. Health checks and reconnects run
independently across servers, while duplicate reconnects for one server share a
single attempt. A server busy with a tool call is skipped for that health cycle,
not treated as degraded. **Reconnect and refresh tools** explicitly starts a
fresh session and rediscovers its tool list. The runtime does the same after the
server emits `notifications/tools/list_changed`.

Saving a candidate connects every enabled server before replacing the current
set. If validation or initialization fails, the previous set remains active.
Each running turn holds an immutable registry snapshot, so a configuration or
tool-list change applies only to subsequent turns.

Discovery is fail-closed and bounded. Mounted names must fit the provider-safe
64-byte `[A-Za-z0-9_-]` contract after namespacing. OpenWave caps JSON-RPC frame
size, tool count, pagination, cursors, descriptions, individual schemas, and
aggregate tool metadata before publishing a connection. A server that exceeds a
limit stays out of the active tool set and receives only a fixed diagnostic in
Settings.

## MCP App views

A server may declare an [MCP Apps](https://github.com/modelcontextprotocol/ext-apps)
view for a tool through `_meta` (`ui.resourceUri`, or the legacy flat
`ui/resourceUri` spelling). OpenWave validates the declaration at discovery —
it must be a bounded, control-character-free `ui://` URI; a malformed
declaration fails the connection — and prefetches the document once per
connection through `resources/read`, bounded at 1 MiB.

When such a tool completes successfully, its transcript card renders the
declared view. The renderer event stream itself carries only a typed
reference (the configured server namespace and the validated URI). The
renderer never holds the markup at all: it trades its bearer for a
single-use, minute-lived frame token, and the iframe loads the document from
the host, which serves it with its own strict Content-Security-Policy — an
http-served frame does not inherit the app's policy the way a `blob:` or
`srcdoc` document would, so the view's inline script runs while its network
egress stays shut. The frame is sandboxed with `allow-scripts` only and is
never same-origin with the app: it has no access to OpenWave's DOM, storage,
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

## Headless bootstrap

`openwave serve` can still read an initial configuration from the JSON file
named by `OPENWAVE_MCP_CONFIG`:

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
silently narrowing the advertised tools.
