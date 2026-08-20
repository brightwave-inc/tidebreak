# 49. Watch-and-Fix Is a Durable Server-Side Task

- Status: Accepted
- Date: 2026-08-20
- Owners: code mode
- Related: [`0009-queued-turns.md`](0009-queued-turns.md),
  [`0030-code-mode-separate-surface.md`](0030-code-mode-separate-surface.md),
  [`0033-code-mode-approvals.md`](0033-code-mode-approvals.md),
  [`0035-code-mode-wire-contract.md`](0035-code-mode-wire-contract.md),
  [`0042-user-initiated-pr-merge.md`](0042-user-initiated-pr-merge.md)

## Context

"Watch and fix" was a prompt: the workflow control composed a long
instruction into the workspace's interactive session and asked the engine to
loop — poll checks, fix failures, rebase, and keep watching. That shape has
structural problems no prompt wording can fix. The watch dies with the turn,
the session, or the app. It occupies the user's conversation for hours. Its
polling burns engine context on waiting. And the prompt asked the agent to
enable auto-merge through its own `gh`, side-stepping the merge boundary
[record 42](0042-user-initiated-pr-merge.md) draws.

Everything a durable watch needs already exists: sessions recover across
restarts with spawn-epoch fencing, turns can be driven programmatically
through the session worker, the PR digest path reads host state with
head-SHA consistency, and [record 9](0009-queued-turns.md) established the
try-based sweep as the cheap durable-driver idiom.

## Decision

Watch and fix is a server-side task backed by a `code_watch` row and driven
by a try-based sweep. The sweep reads its work list from the table every
tick, so a restart resumes every active watch with no extra recovery state.

The watch owns a dedicated session — `code_session.kind = watch` — in the
same worktree as the conversation it forked from. One active session per
workspace becomes one per *kind*: the interactive conversation and the watch
coexist, and the sweep submits a fix turn only while no session in the
workspace is running a turn, so the two never contend for the worktree.
Watch sessions run `auto`; approvals they raise park in the normal approval
flow. List surfaces keep showing the interactive session: watch digests
carry their kind and never displace the conversation in the updates store.

Each sweep classifies the fresh PR digest and takes exactly one step:
submit a bounded fix turn (failing checks, conflicts, behind, requested
changes), wait (pending checks, merge queue), park with `NeedsYou` (draft,
review required, a fix attempt that did not move the head), or finish
(merged, closed, ready). The fix instruction is scoped to a single cycle;
the loop lives in the sweep, not in the engine's context window.

The watch never merges, never arms auto-merge, and never marks a draft
ready. Those are PR state changes record 42 reserves for the user; the
watch reports "ready to merge" and stops.

## Consequences

- A watch survives app restarts, engine crashes, and closed laptops that
  recover; its history is ordinary turns in an ordinary session journal.
- The digest gains a `kind`, the PR digest gains `head_sha`, and the PR
  snapshot gains a `watch` block; clients that predate them parse on.
- A fix turn that leaves the head unchanged parks the watch instead of
  looping — the escalation is visible, not silent retry.
- The interactive composer is free while the watch runs; agent-prompt
  workflow actions hide while a watch is active so two agents never edit
  one worktree at once.
