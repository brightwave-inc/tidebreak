# 74. Agent MCP drives code mode over the attach contract

- Status: Proposed
- Date: 2026-08-26
- Owners: cli
- Related: [`0073-agent-mcp-drives-chat-over-attach.md`](0073-agent-mcp-drives-chat-over-attach.md),
  [`0033-code-mode-approvals.md`](0033-code-mode-approvals.md),
  [`0069-durable-code-session-queue.md`](0069-durable-code-session-queue.md),
  [`docs-site/content/docs/headless.mdx`](../../docs-site/content/docs/headless.mdx)
- Supersedes: none

## Context

`tidebreak agent-mcp` already drives chat over the attach contract: a
`ToolRegistry` of schema-discoverable tools, `AutoApproveGate` so possession of
the bearer is the authorization boundary, and one return shape for run-turn /
wait / decide (`completed`, `needs_approval`, `running`, …). Code mode has the
same attach surface — repos, workspaces, sessions, a durable turn queue,
foreign harness approvals, diffs, and git/PR verbs — but an external agent
that wants to drive it still has to reverse-engineer `/code/*`.

The chat decision (record 73) already
said later surfaces add a file and one registration call. This is that call.

## Decision

**1. Code mode is driven through the same `tidebreak agent-mcp` process.**
A `code.rs` module registers snake_case tools with hand-authored JSON schemas
(`additionalProperties: false`). Reads are `ReadOnly`; mutations are
`Sensitive`. The MCP gate still auto-approves *calling our tools*. It does not
auto-approve the driven session's harness approvals.

**2. The chat return contract extends with `queued`.**
`code_run_turn`, `code_wait`, and `code_decide` return the chat shape
`{status, assistant_text, pending?, events_cursor}` with one added status:
`queued`. `POST /code/sessions/{id}/turns` waits for the worker to finish or
to park the message (decision 69). A `SubmitTurnResponse::Queued` receipt
returns immediately with the queue position and the turn id the promoted turn
will run under. The caller re-checks with `code_wait`. `running` still means
the follow timeout elapsed while the turn continues server-side.

`code_run_turn` subscribes the session event socket *before* submitting, so a
live approval cannot hide behind the long POST. Reconnect climbs the shared
event-stream ladder and resumes from the cursor; a socket that cannot be
reopened is reconciled through `list_session_turns` (and the durable queue
when the turn has not been promoted yet).

**3. Harness approvals are interaction points. They are never auto-approved.**
A turn that parks on `approval_requested` returns `needs_approval` with the
approval id. `code_decide` is the only path that settles it. This is the same
stance as chat: the MCP bearer authorizes driving Tidebreak, not bypassing the
engine's permission system (decision 33).

**4. Git and pull-request actions are plain Sensitive tools.**
`code_git_commit`, `code_git_push`, and `code_git_pr` are not approval-gated
beyond the MCP `Sensitive` class. The bearer already authorizes the workspace
the same way `tidebreak code git …` does. Putting a second consent card on
those verbs would invent a gate the interactive CLI does not have.

Deliberately skipped: `run_action` (named quick actions) and `reap_session`.
They do not fall out of the session → turn → decide → diff/git loop.

## Alternatives Considered

**Document the `/code/*` routes and let harnesses call them.** Same rejection
as chat: there is no OpenAPI document, subscribe-before-post and the durable
queue are easy to get wrong, and every harness would reimplement them.

**Auto-approve harness approvals from the MCP gate.** That would make
`agent-mcp` a permission bypass for foreign engines, which decision 33
forbids.

**Park git/PR behind `code_decide`.** Those verbs are operator actions on a
workspace the bearer already holds, not harness-requested tool calls. The
interactive CLI runs them without an approval card.

**Block on a queued submit until it runs.** That would hide queue position
from the caller and make a second `code_run_turn` look like a hang. Returning
`queued` and following with `code_wait` keeps the chat contract's "timeout
means `running`, not `failed`" honesty.

## Consequences

An MCP-speaking harness can drive a code session end to end: register a repo,
open a workspace, run turns, answer approvals, inspect the diff, and commit or
open a PR. Callers must handle `queued` in addition to the chat statuses; a
harness that only reads `completed` will stall on the first follow-up sent
mid-turn.

Revisit if git/PR should require an extra confirmation even from a bearer that
already owns the workspace, or if `run_action` / `reap_session` become part of
the headless loop.

## Validation

`crates/tidebreak-cli/tests/agent_mcp_code.rs` speaks MCP over the real binary
against `tidebreak serve` with the feature-gated scripted harness: repo →
workspace → session → `code_run_turn` completes with assistant text; a
scripted approval returns `needs_approval` and `code_decide` settles it; a
submit while a turn runs returns `queued` and `code_wait` drains both; after a
scripted worktree edit, `code_diff` and `code_git_status` return sane shapes.
The registry unit test compiles every advertised input schema, including the
new code tools. A plausible wrong implementation — auto-approving the parked
write, treating a queued submit as `failed`, or diffing before the edit lands
— fails those tests.
