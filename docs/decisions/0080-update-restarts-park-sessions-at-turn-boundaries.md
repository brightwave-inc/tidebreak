# 80. Update restarts park sessions at turn boundaries

- Status: Accepted
- Date: 2026-09-01
- Owners: desktop updater, code mode, chat runtime
- Related: [`0043-ship-arm64-packages-and-enable-cross-platform-updates.md`](0043-ship-arm64-packages-and-enable-cross-platform-updates.md),
  [`0064-idle-engine-children-are-parked.md`](0064-idle-engine-children-are-parked.md),
  [`0069-durable-code-session-queue.md`](0069-durable-code-session-queue.md),
  [`0032-code-workspaces-worktrees-checkpoints.md`](0032-code-workspaces-worktrees-checkpoints.md)
- Supersedes: none

## Context

Restart-to-update ([`0043`](0043-ship-arm64-packages-and-enable-cross-platform-updates.md))
quiesces only the host-broker sidecar before replacing the bundle. Nothing
brings session work to a safe point first, and the exit path makes that
expensive:

- Code engine children run in their own process groups, and the restart exits
  through `app.exit`, so their kill-on-drop guards never run. Every child
  survives as a live orphan. Boot recovery
  (`crates/tidebreak-server/src/code/recovery.rs`) then fences each session
  `OrphanAlive`, which blocks its whole workspace until the user reaps it by
  hand ([`0055`](0055-multiple-sessions-per-workspace.md)). Updating with open
  code sessions lands in the worst recovery branch there is.
- Chat turns are durably leased with a 60-second expiry. The dead process's
  leases are not handed back, so the relaunched worker waits out up to a
  minute of dead time before it re-claims and resumes each mid-flight turn.

Everything needed to do better already exists. Idle park
([`0064`](0064-idle-engine-children-are-parked.md)) proves every engine child
can be released between turns and rebuilt from a stored resume ref for about
0.7 s of wake latency. The durable code queue ([`0069`](0069-durable-code-session-queue.md))
survives restarts. Chat's lease protocol already recovers from a process that
dies mid-turn by rebuilding the transcript and re-running only the model call
in flight. What no engine supports is resuming *mid-turn*: a code turn's
boundary is the only safe handoff point.

## Decision

The explicit restart-to-update brings the process to a safe point before the
bundle is replaced, through a quiesce handle the embedded server exports
(`crates/tidebreak-server/src/update_quiesce.rs`) and the desktop calls
inside the existing install barrier, before the broker drain.

Code mode parks at turn boundaries:

- A process-wide quiesce flag holds every session worker's queue drain and
  refuses any turn start that wins the worktree lock after the flag flips. A
  send during the quiesce parks as a durable queue row — the same answer a
  busy workspace gives — and runs after the relaunch.
- Idle engine children park immediately through the 0064 path, which clears
  the recorded pid, so recovery has nothing to fence. In-flight turns run to
  completion and their workers park on the way back to idle.
- The quiesce waits up to 20 s for in-flight turns to reach their boundary,
  and proves the boundary through the worktree locks themselves: after the
  flag is up it acquires and releases every workspace's turn lock, so a
  start that raced the flag has either finished or will re-read the flag
  under its own acquisition and refuse. A turn still running at the
  deadline fails the quiesce: the update stays staged, the panel shows a
  retryable message, and admission reopens. A code turn is never
  interrupted for an update, because no engine can resume one.

Chat drains and hands leases back:

- The turn worker stops claiming, gives running turns a 5 s grace to finish,
  then aborts the rest and sets each aborted claim's lease expiry to now. An
  abort is exactly the crash the lease protocol recovers from; handing the
  lease back only removes the up-to-60-s wait, so the relaunched worker
  re-claims immediately.

Deliberately excluded: ordinary quit (this covers only restart-for-update),
MCP server children, sandbox agent-run workers (durably leased and
crash-tolerant already), remote code sessions (their engine lives in a
sandbox and survives on its own), and auxiliary terminals
([`0036`](0036-code-mode-auxiliary-terminals.md) keeps them ephemeral).
Installation stays an explicit user action; nothing here installs silently.

## Alternatives Considered

- **Do nothing.** Updating with open code sessions orphans children and
  fences workspaces; users learn to fear the restart button, and updates are
  deferred. That is the problem, not an option.
- **Interrupt in-flight code turns at the deadline.** Converts a slow update
  into lost agent work; an interrupt discards the turn's remaining plan.
  Failing the quiesce with a retryable message loses nothing.
- **Kill children silently and let recovery fence.** Today's behavior with
  extra steps; reaping is manual and workspace-blocking.
- **Split the engine into a daemon that outlives the UI.** The real
  zero-interruption end state, and what remote sessions already do — but it
  fights the one-server-per-data-directory advisory lock and the
  native-only host authority (broker, keychain, client executor). The turn
  boundary barrier gets most of the value for a fraction of the surface;
  this record does not preclude the daemon later.
- **Let chat turns run to completion like code turns.** A chat turn can run
  for minutes and, unlike code, is provably resumable mid-turn; waiting adds
  restart latency for no safety gain.

## Consequences

- Restart-to-update can now refuse, with a message, while a code turn runs
  long. The staged update remains installable; the user retries at the next
  boundary.
- Sends that race the install window queue durably rather than failing;
  users see them run after the relaunch.
- The quiesce flag is one more admission gate every code turn start passes;
  new turn-start paths must check it (the worktree-lock check catches any
  that forget, since starts all take the lock).
- Revisit when engines gain mid-turn resume (the deadline and refusal can
  then disappear), or if a daemon split makes the barrier unnecessary.

## Validation

- `an_update_quiesce_refuses_new_turns_until_resumed`
  (`crates/tidebreak-server/src/code/session_worker.rs`): while the flag is
  up a turn that wins the worktree lock is refused `UpdateQuiesced` and no
  turn row is written; clearing the flag lets the same send run. A wrong
  implementation that only gated the queue drain would still start live
  sends and fail this.
- `an_expired_lease_handback_makes_the_turn_immediately_reclaimable`
  (`crates/tidebreak-core/src/db/tests.rs`): the handback is token-exact —
  a wrong token leaves the live lease untouched, the exact token makes the
  turn claimable at once, and a superseded token no-ops.
- The updater's barrier ordering tests
  (`crates/tidebreak-desktop/src/updater.rs`) pin quiesce → install →
  shutdown, with resume only on a failed install.
- Manual: restart-to-update with an idle code session, with a mid-turn code
  session (expect the retryable refusal), and with a streaming chat turn
  (expect the turn to continue shortly after relaunch).

## Amendment (2026-09-02)

`HarnessCaps::mid_turn_resume` is the flag that distinguishes the two
quiesce halves. The internal engine declares `Supported`: an update abort
hands the `code_turn` lease back and boot recovery leaves a pid-less
running turn for the claim scan. Every other adapter declares
`Unsupported` and still waits for a turn boundary. D4a keeps both halves
wired; D4b unifies them on this flag.


## Amended 2026-09-10

PR #3156 renamed the shared code-mode tables to `session`, `turn`, `event`, and `approval`, and their id types to `SessionId`, `TurnId`, and `ApprovalId`; historical names above refer to those current names.

## Amended 2026-09-23: quitting reuses the quiesce

This record left ordinary quit out. Quitting with agents working lost work the
same way an update restart did: chat turns sat on their leases until the next
launch, and code engine children were orphaned and then fenced. So a quit now
uses the same handle.

- A quit counts the agents mid-turn first (`UpdateQuiesce::working_agents`).
  With none, the app quits at once. With some, the desktop asks the person to
  stop them, wait for a safe point, or cancel.
- Waiting for a safe point runs this quiesce with no code deadline
  (`quiesce_for_quit`): code turns run to their boundary, idle children park,
  and chat leases are handed back so the next launch resumes those turns.
  While it waits, the person can still stop the agents or cancel, and a
  cancel reopens admission.
- Stopping cancels chat turns durably, the way the Stop button does,
  interrupts code turns on external engines, and parks what is left for at
  most 15 seconds (`stop_for_quit`). Unlike an update, those chat turns do not
  resume after the next launch, because the person asked to stop them.
- A logout, restart, or shutdown quits without asking, so Tidebreak never
  holds up the system. The restart that installs an update never shows the
  prompt; it keeps its own deadline, refusal, and order inside the install
  barrier, all unchanged.
- An update and a quit can overlap: the person can press Cmd+Q while an
  update quiesces, or restart to update while a quit waits. Each holds the
  quiesce on its own account and releases only its own hold, and admission
  reopens once neither holds. Cancelling a quit never lowers an update's
  quiesce.
- A code turn parked on an approval, a question, or a plan never reaches its
  boundary on its own. The prompt counts those turns as waiting for the
  person and offers the inbox, and a quit that waits for a safe point shows a
  bar instead of a dialog, so the person can answer while it waits.
- The count a waiting quit shows is exactly what the quiesce waits on: every
  local session mid-turn or holding an engine child that has not parked yet
  (`safe_point_progress`). The count that decides whether to ask leaves out
  idle children, which are not working.
- A count, stop, or wait that panics or hangs cannot strand a quit. A count
  that fails lets the quit go ahead, a stop quits after 30 seconds at most,
  and a wait whose quiesce panics releases its hold and asks again.
