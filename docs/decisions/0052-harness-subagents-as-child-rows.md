# 52. Harness Subagents Appear as Child Rows

- Status: Accepted
- Date: 2026-08-20
- Owners: code mode
- Related: [`0031-code-mode-harness-adapters.md`](0031-code-mode-harness-adapters.md),
  [`0035-code-mode-wire-contract.md`](0035-code-mode-wire-contract.md),
  [`0050-watch-and-fix-is-a-durable-task.md`](0050-watch-and-fix-is-a-durable-task.md)

## Context

Engines fan work out to subagents: Claude Code spawns them through the `Task`
tool and tags every event a subagent produces with a top-level
`parent_tool_use_id`; Grok exposes `spawn_subagent` and companion commands.
Tidebreak currently flattens all of it. The Claude adapter never reads
`parent_tool_use_id`, `tool_detail()` has no `Task` branch, and `CodeEvent`
has no notion of nesting — a subagent's tool calls and text interleave with
the parent's, indistinguishable in the transcript and invisible on the rail.

The rail now shows watch sessions as child rows under their workspace card
(the sidebar half of record 50). Harness subagents should read the same way:
a thing running under the conversation, with a status, that you can open to
see what it is doing. That requires per-harness parsing, so it needs a
contract before code.

## Decision

Subagents are derived, not declared. `CodeEvent`'s tool and assistant
variants gain an optional `parent_call_id`; a subagent *is* the span of its
parent `Task` call — the call's start is the subagent's start, the call's
result is its end and outcome. No `subagent_started`/`subagent_completed`
event pair: a separate lifecycle would be one more thing to keep true across
harness restarts, and the spanning call already carries name, input, status,
and result. Old journals replay unchanged; events without `parent_call_id`
are the parent's own.

Subagents are not sessions. They get no session row, no digest of their own,
and never touch the conversation's slot in the updates store — record 50's
displacement rule extends to them. The server cannot steer or resume them;
pretending they are sessions would promise exactly that. Rail visibility
rides the parent session's digest as a bounded enrichment
(`subagents: [{ call_id, name, status }]`, capped, optional), the same
additive pattern that carried watch state onto the digest.

A subagent row shows the Task's name or description and a status derived
from the spanning call (running, done, failed). Opening it filters the
parent transcript to events whose `parent_call_id` matches — a view inside
the parent session, not a new socket.

Claude Code's `Task` family and Grok's `spawn_subagent` family both map onto
this contract. Grok normalizes the launcher around its durable `subagent_id`,
then attributes output, wait, and kill activity to that spanning call. Codex
and OpenCode map when their streams expose equivalent structure.

Parent lifecycle is the outer bound. A normal parent turn completion settles
any still-running child as done; failure or interruption settles it as failed.
Recovery and fencing paths also fail any child left running by a missing or
orphaned parent process. An already-settled child is never overwritten.

## Consequences

- The transcript can collapse a subagent's noise behind one row and let the
  reader expand it — the flattened interleaving goes away.
- The wire contract grows only optional fields; a UI ahead of or behind the
  server degrades to today's flattened view.
- Deriving subagents from spans keeps one source of truth while parent terminal
  events and recovery fencing prevent a crash mid-Task from leaving a child
  permanently running.
- Each harness pays its own mapping cost at the adapter boundary
  (record 31); nothing downstream of `CodeEvent` knows harness names.

## Alternatives rejected

- **Per-subagent sessions.** The harness owns their lifecycle; the server
  could neither steer nor resume them, and every session surface (attention,
  digests, recovery) would carry rows it cannot honor.
- **Lifecycle events for subagents.** A second source of truth beside the
  spanning call, and one more invariant to re-derive after a restart.
- **Client-side heuristics.** Grouping by message ordering breaks on replay
  and differs per harness; the journal is the contract, so the linkage must
  be in it.
