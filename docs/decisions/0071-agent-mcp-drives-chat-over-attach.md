# 71. Agent MCP drives chat over the attach contract

- Status: Proposed
- Date: 2026-08-26
- Owners: cli
- Related: [`0007-cli-headless-feature-parity.md`](0007-cli-headless-feature-parity.md),
  [`0012-data-dir-listen-endpoint.md`](0012-data-dir-listen-endpoint.md),
  [`0048-one-interaction-model.md`](0048-one-interaction-model.md),
  [`docs-site/content/docs/headless.mdx`](../../docs-site/content/docs/headless.mdx)
- Supersedes: none

## Context

Tidebreak already exposes chat as HTTP + WebSocket (the same attach contract
`-p` and the desktop use) and already serves MCP over stdio (`tidebreak mcp`
for filesystem tools, `tidebreak browser-mcp` for the browser). An external
agent that wants to drive a running Tidebreak today has to reverse-engineer
those routes: there is no OpenAPI document, the event journal is a WebSocket,
and a turn parks on approvals, plans, and questions that a raw HTTP client has
to notice and answer.

`browser-mcp` is the pattern to copy: a `ToolRegistry` of tools whose
`execute()` calls a loopback client, mounted into `tidebreak_mcp::McpServer`
with an `AutoApproveGate`, served over stdio. Chat needs the same face, with
one difference: it is an attach client like `-p`, so it accepts `--server` /
`--attach` instead of refusing them.

## Decision

**1. Chat is driven through `tidebreak agent-mcp`, not by documenting raw HTTP.**
The subcommand resolves the attach endpoint, builds `api::client::Client`,
assembles a `ToolRegistry`, and serves MCP over stdio. stdout carries only
JSON-RPC; diagnostics go to stderr. Later surfaces (settings, code mode) add a
file and one registration call.

MCP is the right face because any harness that already mounts MCP tools gets
schema-discoverable operations without an OpenAPI document we do not publish,
and because it reuses the `tidebreak-mcp` server face and the `browser-mcp`
precedent rather than inventing a second protocol.

**2. Bearer possession is the authorization boundary for the MCP tools.**
The registry is wired with `AutoApproveGate`, so ReadOnly and Sensitive tools
are listed and callable. That gate is *not* the driven chat's approval gate.
Approvals, plans, and questions the model raises inside the chat are never
auto-approved: they surface as interaction points on the run-turn contract.

**3. `chat_run_turn`, `chat_wait`, and `chat_decide` share one return shape.**

```json
{
  "status": "completed|needs_approval|needs_plan_decision|needs_answer|needs_host_consent|running|cancelled|failed",
  "assistant_text": "…",
  "pending": { "type": "approval", "call_id": "…" },
  "events_cursor": 42
}
```

`chat_run_turn` posts with a client-chosen idempotent `turn_id` after
subscribing the events socket, then follows until a settle, a park, or the
timeout. `running` means the timeout elapsed while the turn continues
server-side (turns are durable); the caller re-checks with `chat_wait` or
`chat_status`. `chat_decide` takes a print-protocol decision object, applies it
over HTTP, and follows to the next settle point.

**4. Host folder consent is not drivable.**
If a turn parks on `request_folder_access`, the tools return
`needs_host_consent` and offer no decision path. Standing folder consent comes
only from `tidebreak folder connect` or the desktop. This matches print mode:
the driving protocol's closed vocabulary is approval, plan, and questions.

## Alternatives Considered

**Document the HTTP + WebSocket routes and let harnesses call them.** There is
no OpenAPI document, and there will not be one: the route table is the
specification. Every harness would reimplement subscribe-before-post, the
reconnect ladder, durable-turn reconciliation, and the interaction vocabulary.
MCP tools carry those rules once.

**Extend `tidebreak -p` instead of a long-lived MCP server.** Print mode is
one turn and one process. An external agent that wants to create chats, run
several turns, answer an approval, and inspect the journal needs a session,
not a fresh argv each time.

**Auto-approve the driven chat's tool calls from the MCP gate.** That would
make `agent-mcp` a permission bypass. The MCP gate authorizes *calling our
tools*; the chat's own approvals stay interaction points.

**Let `chat_decide` grant `request_folder_access`.** Folder access is
host-machine consent, not a chat decision. Print mode already refuses to treat
it as an `Interaction`. Opening a grant path here would mint standing consent
from a bearer token.

## Consequences

An MCP-speaking harness can drive chat on a running Tidebreak without learning
the attach routes. The tool list will grow (settings, code mode) through the
same registry. Callers must handle `running` and the three interaction
statuses; a harness that only reads `completed` will stall on the first
approval.

Revisit if we publish a real OpenAPI surface, if folder consent gains a
headless grant path that is not `tidebreak folder connect`, or if MCP itself
is the wrong face for long-running follow.

## Validation

`crates/tidebreak-cli/tests/agent_mcp.rs` speaks MCP over the real binary
against `tidebreak serve` with the scripted provider: a turn completes with
assistant text; an exec approval returns `needs_approval` and `chat_decide`
settles it; `chat_events` returns frames after a cursor; a short timeout
returns `running` and `chat_wait` completes the same turn. A unit test
compiles every registered `ToolSpec` input schema as JSON Schema. A plausible
wrong implementation — auto-approving the parked exec, treating timeout as
`failed`, or skipping the event cursor — fails those tests.
