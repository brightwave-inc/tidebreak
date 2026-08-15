# Harness fixtures

Adapter parsers may only be written or modified against captured streams from
a real engine invocation. Each capture lives under
`<harness>/<version>/` with:

- `<scenario>.ndjson` — the raw protocol stream, one JSON object per line
- `<scenario>.expected.json` — the normalized `HarnessEvent` sequence the
  parser must produce
- `manifest.toml` — exact argv, observed version, date, and redaction notes

CI replays fixtures. It cannot capture them.

## Capture

The engine must be installed and signed in on the capturing machine. From the
workspace root:

```text
cargo run -p tidebreak-harness --features capture --bin tidebreak-harness-capture -- \
  --harness claude-code \
  --scenario plain-text \
  --prompt "reply with exactly: hello from fixture"
```

The binary creates a throwaway git repo, runs the engine in print mode, tees
stdout to `fixtures/<harness>/<version>/<scenario>.ndjson`, and writes
`manifest.toml`.

Use the cheapest model the engine accepts, tiny prompts, and the smallest
tool allowlist that still produces the scenario (for example a single `Read`
for a tool-use turn). Check `claude --help` for current flags; the capture
bin defaults to `-p --output-format stream-json --verbose --include-partial-messages`.

### Codex CLI

`--harness codex` drives the long-lived `codex app-server --stdio` JSON-RPC
child (0.147.0). Fixtures are framed both-direction exchanges, one object
per line:

```text
{"dir":"out","msg":{"id":1,"method":"initialize","params":{…}}}
{"dir":"in","msg":{"id":1,"result":{…}}}
```

That path was chosen over `codex exec --json` because app-server is the
richer approval channel (`item/commandExecution/requestApproval`). Probe
`codex app-server --help` and `codex exec --help` before recapturing a new
version; if app-server is gone or unstable, recapture via exec JSONL and
update the version manifest.

The capture bin writes initialize → thread/start → turn/start and tees
until `turn/completed`. Approval request/response pairs for
`approval-approve` / `approval-deny` were captured with a helper that
completes `item/commandExecution/requestApproval` with `{decision: accept}`
or `{decision: decline}`.

Re-capture the whole version directory when the engine version moves.

### opencode

`--harness opencode` drives a long-lived `opencode serve --hostname 127.0.0.1 --port N` child (1.18.18). Fixtures are framed both-direction HTTP + SSE exchanges, one object per line:

```text
{"dir":"out","msg":{"kind":"http","method":"POST","path":"/session","body":{…}}}
{"dir":"in","msg":{"kind":"http","status":200,"path":"/session","body":{…}}}
{"dir":"in","msg":{"kind":"sse","event":{"type":"session.created",…}}}
```

That path was chosen over `opencode run --format json` because serve is the richer channel: sessions, `prompt_async`, directory-scoped `/event`, and `POST /permission/{id}/reply`. Probe `opencode serve --help` and `opencode run --help` before recapturing a new version.

The capture bin writes POST `/session` → POST `/session/{id}/prompt_async` and tees `/event?directory=…` until `session.idle` or `session.error`. Approval request/response pairs for `approval-approve` / `approval-deny` were captured with a helper that completes `POST /permission/{id}/reply` with `{reply: once}` or `{reply: reject, message: …}`.

Re-capture the whole version directory when the engine version moves.

## Redaction

Before committing a capture:

1. Strip absolute home paths (`/Users/…`, `/home/…`). Replace the worktree
   with `/workspace` when the path is only a cwd.
2. Strip anything token-like: API keys, bearer tokens, `sk-…` strings,
   cookie headers, thinking signatures.
3. Replace host-local sockets and paths (`/var/folders/…`,
   `$TMPDIR/…/*.sock`) with `/tmp/redacted.sock`.
4. Keep structural fidelity: event `type`s, tool names, session ids that the
   resume fixture needs, and argument *shapes*.
5. Record every redaction in `manifest.toml` under `redaction_notes`.

Real streams include user-hook `system` events (`hook_started`,
`hook_response`). Leave them in. The parser must tolerate them.

After redaction, regenerate expected sequences:

```text
UPDATE_HARNESS_FIXTURES=1 cargo test -p tidebreak-harness --locked
```

Do not invent parser branches for shapes that are not in a fixture.

## Approval channel (Claude Code 2.1.233)

`--permission-prompt-tool` is a hidden flag: it is not listed in `--help`,
but an unknown flag errors and this one does not. Print-mode approvals ride
an MCP tool registered via `--mcp-config`. HTTP transport with a Bearer
token was captured; that is the loopback the server serves.

`approval-request.mcp.json` is the CLI→MCP `tools/call` payload. The
matching `approval-request.ndjson` is the print-mode stream parked at the
Write tool-use, before a decision. `approval-allow` and
`approval-deny-with-feedback` are the full streams after the captured
responses `{"behavior":"allow"}` and
`{"behavior":"deny","message":"no — use the fixtures directory instead"}`.
The deny message is what the model reads as the tool_result.
