# 54. Multiple Agent Sessions Share One Workspace

- Status: Accepted
- Date: 2026-08-20
- Owners: Code mode
- Related: [0009](0009-queued-turns.md),
  [0030](0030-code-mode-separate-surface.md),
  [0050](0050-watch-and-fix-is-a-durable-task.md),
  [0053](0053-code-worktrees-live-in-a-user-visible-root.md)

## Context

A code workspace held one interactive agent. `create_session_of_kind` rejected
a second one with a `session_exists` conflict, and the desktop client mirrored
the rule by keying its live digest map on the workspace alone.

The rule was never about the agent. It was about the checkout. A workspace is
one git worktree, and two harnesses editing the same files at the same time is
corruption, not concurrency: they overwrite each other's edits, and their
per-turn checkpoints race for `.git/index.lock`. Allowing one session was the
cheapest way to make that impossible.

It is also the wrong shape for how people work. Running a second agent on the
same branch — a different harness for a second opinion, a scratch conversation
that should not disturb the main thread, a fork of a transcript into a fresh
context — needs several conversations over one checkout, not several checkouts.
Cloning a worktree per conversation would give each agent a private tree, at
the cost of the shared state that made them worth running together.

Watch sessions already proved the shape. Record 50 put a watch session in the
same worktree as the conversation it forked from, and kept it safe by making
the watch sweep wait while an interactive turn was running. The workspace has
carried two sessions since then. What it has not carried is two sessions that
both take turns on their own schedule.

## Decision

A workspace holds any number of interactive sessions and at most one watch
session. Turns across all of them are serialized on the worktree.

The turn lock is a per-workspace async mutex held by a session worker for the
length of a turn, from the moment the turn row is written to the moment the
harness reports an outcome. A worker whose turn is queued behind another waits
on the lock rather than running, so the guarantee holds no matter which route
started the turn — a user send, a queued follow-up, or the watch sweep.

`submit_turn` treats a busy sibling the same way record 9 treats a busy
session: the message parks in the session's single follow-up slot and the
route answers `Queued`. The reader sees a queued message, not a stalled
request, and the worker runs it when the worktree frees.

Concurrency lives in the conversation list. The filesystem stays single-file.

## Alternatives Considered

**A worktree per session.** Every conversation gets its own checkout, so turns
run in parallel with no lock at all. Rejected: it changes what a workspace is.
The branch, the diff, the pull request, and the review surface are all keyed to
one tree, and splitting the tree means merging those back together — a much
larger change than the problem asks for, for a benefit (parallel edits) that
agents on one task rarely want.

**Reuse the watch sweep's existing check.** The sweep already skips a cycle
when another session in the workspace is running, so it looked like the
serialization was there to reuse. It is not: the check is one-directional and
advisory. It makes watch defer to interactive, never the reverse, and it lives
in the sweep rather than on the turn path, so two interactive sends would sail
straight past it. The check stays as a cheap pre-filter that avoids waking a
harness that would only wait; the lock is what makes it a rule.

**A queue per workspace instead of per session.** One ordered queue for the
whole worktree, drained by whichever worker is free. Rejected: it moves the
follow-up slot away from the session that owns it, so a reader could no longer
see which conversation their queued message belongs to, and cancelling one
session's queued turn would mean reaching into a shared structure.

## Consequences

- A second agent starts immediately, but its first turn may wait. The wait is
  visible as a queued message, and it is bounded by the sibling's turn.
- A worker waiting on the lock is not reading its command channel, so a
  shutdown or an interrupt aimed at a queued session applies after the sibling
  turn ends. The session is not running at that point, so there is nothing in
  flight to lose; the worker re-reads the session before it starts, and a
  session ended during the wait never takes its turn.
- The `session_exists` conflict now fires for watch sessions only. Clients that
  read it as "this workspace is taken" need the session list instead.
- Per-workspace client state keyed on the workspace alone becomes wrong. The
  desktop digest map moves to workspace → session.

## Validation

`cargo test -p tidebreak-server code::` covers the create guard for both kinds
and the serialization: two interactive sessions in one workspace create
cleanly, a turn submitted while a sibling is running comes back `Queued`, and
the watch guard still refuses a second watch.
