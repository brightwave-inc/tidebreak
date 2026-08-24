# 64. Idle Engine Children Are Parked and Resumed on Demand

- Status: Accepted
- Date: 2026-08-24
- Owners: code mode, harness integration
- Related: [`0031-harness-adapter-boundary.md`](0031-harness-adapter-boundary.md),
  [`0055-multiple-sessions-per-workspace.md`](0055-multiple-sessions-per-workspace.md),
  [`0057-one-claude-child-per-session.md`](0057-one-claude-child-per-session.md),
  [`0059-workspace-reclaim-tiers.md`](0059-workspace-reclaim-tiers.md)

## Context

The product goal is dozens of sessions on one machine. What that costs is not
the adapter boundary — pipes, NDJSON parsing, and the journal are bounded and
cheap — it is the engine children themselves. Each one is a full JavaScript or
native runtime holding a session's context, and today their number tracks
*open* sessions, not *active* ones:

- The Codex and opencode adapters spawn their server child inside `attach`,
  before any turn exists. Only Claude Code spawns lazily on the first turn
  ([`0057`](0057-one-claude-child-per-session.md) point 8); Grok has no
  between-turn child at all.
- Boot re-attaches a worker for every non-ended, non-fenced session,
  concurrently and without a cap (`crates/tidebreak-server/src/code/runtime.rs`,
  `recover`). With eager adapters that is one engine runtime per restored
  session at every app launch, whether or not the user touches any of them.
- No child is ever released while its session stays open. [`0057`](0057-one-claude-child-per-session.md)
  names this as its revisit condition: "if holding a child for a session's
  lifetime proves too expensive in memory across many open sessions."

The wake price is known. 0057 measured spawn-and-resume at roughly 0.7 s to
first engine output, and measured the token cost of a respawn as a rewrite of
the cached prompt prefix — significant only when the worktree changed since
the cache was written. Both costs shrink to the latency alone once the
provider's prompt cache has expired on its own: Anthropic documents a
5-minute default TTL, and a warm child past that window pays the same cache
re-creation on its next turn that a parked one does. Past the cache window,
holding the process buys nothing but resident memory.

[`0059`](0059-workspace-reclaim-tiers.md) already answered the same question
for worktrees: reclaim is safe when what is dropped can be rebuilt from what
is kept. Every engine here can rebuild a live child from a stored ref —
Claude Code with `--resume`, Codex with `thread/resume`, opencode by reopening
its session id — and 0057 already requires the Claude adapter to replace a
dead child through exactly that path before every turn.

## Decision

**Engine children are a reclaim tier, not a session invariant.** A session
owns a resumable ref; the child is cache.

1. **Every adapter spawns its child lazily, on the first turn.** 0057 point 8
   becomes the rule for all adapters, not a Claude special case. `launch`
   validates and constructs; the first `run_turn` spawns and completes the
   engine handshake. Boot re-attach therefore launches no engine processes,
   which dissolves the unbounded-restore concern that kept 0057 from eager
   spawn rather than bounding the loop.
2. **`HarnessSession` gains `park`.** Parking stops the engine's processes
   while the session object stays attachable; a later `run_turn` transparently
   respawns and resumes from the session's ref. The default implementation
   does nothing — Grok's per-turn child and the scripted test engine have
   nothing to release. Claude parks by retiring its channel, the machinery
   dead-child replacement already uses. Codex and opencode terminate the
   server child and re-run their handshake on the next ensure.
3. **The worker parks an idle engine.** The between-turns loop in
   `session_worker.rs` arms a timer only while the engine reports a live
   child pid. After `PARK_AFTER_IDLE` — 15 minutes, three times the
   documented cache TTL, so a wake in practice pays latency and not tokens —
   with no turn, no queued follow-up, and no command, the worker parks the
   engine and clears the session row's `child_pid`, so nothing reads a dead
   pid as live. Resident children then track sessions doing work.
4. **Parking is invisible on the wire.** No new event, no lifecycle state.
   The wake turn may journal another `session_started` — the same repeat a
   dead-child replacement already produces, which consumers fold (0057).
5. **Posture survives the park.** Claude composes its permission mode into
   the respawn argv; Codex restates posture on the first turn after a resume
   (`posture_unsent`); opencode's posture is immutable per engine session and
   rides its stored session state. A mode switched while parked lands at the
   next spawn, which the Claude adapter already models.
6. **A thread that never ran a turn is restarted, not resumed.** Codex does
   not persist a thread before its first turn, so the ensure path picks its
   resume target with the same guard `resume_ref` already applies. Parking a
   session whose engine never ran must not fence it on a ref the engine never
   wrote.

Deliberately excluded: parking mid-turn (the timer arm exists only in the
between-turns loop), a user-visible control for the threshold, and any change
to reap, fence, or shutdown semantics.

## Alternatives Considered

**Do nothing.** Rejected by arithmetic. Dozens of open sessions is the stated
goal, each idle child is a resident runtime, and 0057 explicitly deferred
this problem rather than deciding it.

**A global cap on resident children (LRU eviction).** Bounds worst-case
memory even under load, which idle-parking does not. Rejected here: evicting
a child that is mid-turn is wrong, so a cap could only ever evict idle
children — the same set the idle timer reclaims, chosen with a clock instead
of cross-session coordination. A cap can still be layered on later if
genuinely concurrent load appears.

**Park by reaping the worker.** Reuses an existing path. Rejected: the worker
carries the queue slot, the approval endpoint, and the attention contract,
none of which should churn because a session sat quiet; and reap is a
user-facing recovery action, not a background one.

**Return to per-turn children.** The maximally aggressive reclaim. Rejected:
0057 measured why the warm child wins inside the cache window. Parking after
the window keeps that win and drops the cost.

**One shared server child per harness.** Codex `app-server` and opencode
`serve` are multi-session by design, so N sessions could share one runtime.
Parked, not rejected: it is a larger consolidation with real trades — one
crash takes every session on that harness, per-session environment (browser
capfiles, credential profiles) rides process env and would need re-plumbing,
and Claude cannot join. Worth its own record if idle-parking proves
insufficient.

## Consequences

- A present-but-broken Codex or opencode binary now fails the first turn
  instead of session attach. The Claude tier already behaves this way, so
  every surface that reports a failed turn covers it; a missing binary still
  fails at attach through the probe.
- An interrupt aimed at an idle Codex session previously took the app-server
  child and left every later turn failing "engine child is not running"; with
  ensure-on-turn the next turn respawns, and the idle interrupt itself no
  longer touches the process.
- A watch session whose poll cadence exceeds the threshold parks between
  polls and pays a wake per poll. That is the intended trade: memory is freed
  for the whole quiet interval.
- The first turn after a park pays spawn plus handshake latency, in the
  seconds range. Within the cache window it also pays the prefix rewrite 0057
  measured; the default threshold sits past that window.
- `spawn_epoch`, approvals, and recovery are untouched: parking happens
  inside one worker attachment, and an idle row's pid is already ignored by
  recovery (0057) — now it is also cleared rather than stale.

Revisit when a harness appears whose session state cannot outlive its
process, or if wake latency on the reference tier grows enough that the
threshold needs to be adaptive rather than fixed.

## Validation

- Claude adapter: a parked child's pid is gone, and the next turn respawns
  with `--resume` on the same engine session — asserted on the fake engine's
  argv log, beside the dead-child test it mirrors.
- Codex adapter: nothing spawns before the first turn; a stale ref surfaces
  `ResumeLost` from the first turn; a parked thread that ran resumes on a new
  pid with `thread/resume` on the wire; a parked thread that never ran is
  restarted with `thread/start`, never resumed. A wrong ensure that resumes
  the unwritten thread fails against the fake server's own "thread not
  found" answer.
- Worker: with a session-long scripted child, an idle session parks — the
  scripted engine records the call and the row's pid clears — and the next
  turn still runs. The scripted default keeps per-turn pids, so every other
  worker test exercises the disarmed timer.
