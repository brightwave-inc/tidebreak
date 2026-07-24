# Local MCP servers

OpenWave connects to local MCP servers over stdio. In the desktop app, open
**Settings → MCP servers**, add a definition, and choose **Save and verify**.
OpenWave starts the executable directly with the argument array shown in the
form; it never joins the fields into a shell command.

Each server has:

- a stable namespace containing ASCII letters, numbers, `_`, or `-`;
- an executable and zero or more individual arguments;
- an optional working directory;
- a request timeout from 1 to 3,600,000 milliseconds;
- optional literal **non-secret** environment values;
- optional `env_from` names selected from the OpenWave host environment; and
- an enabled switch.

The child environment starts empty. Literal values are ordinary settings and
are visible in the Settings form. Executables, arguments, and working
directories are also ordinary displayed settings, so do not put credentials in
any of those fields. For a credential, set it in the environment that launches
OpenWave and enter only its variable name under **Forward environment names**.
OpenWave resolves that value at process launch; it does not store it in SQLite
or return it to the renderer. A missing selected name produces a server-specific
error containing the name, not a value. Child stderr is discarded so a server
cannot copy a forwarded credential into OpenWave's host logs.

All mounted names use `mcp__{namespace}__{remote_tool}`. MCP tools are sensitive:
the existing OpenWave approval gate must approve each call before it crosses
the process boundary. MCP approvals cannot be remembered for the chat. A server
definition can change behind a stable namespace, so reusing a name-based grant
would silently widen its authority.

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
      "env": {
        "LOG_LEVEL": "info"
      },
      "env_from": ["DOCUMENTS_TOKEN"],
      "request_timeout_ms": 60000,
      "enabled": true
    }
  ]
}
```

The schema is closed, including at the API boundary. Broad process-environment
inheritance is not supported. When there is no saved desktop configuration, a
malformed bootstrap file, missing selected environment name, or failed enabled
server makes startup fail rather than silently narrowing the advertised tools.
