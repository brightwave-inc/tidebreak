# 76. Parallel turns in one code workspace

- Status: Proposed
- Date: 2026-08-27
- Owners: code mode
- Related: [`0032`](0032-code-workspaces-worktrees-checkpoints.md),
  [`0053`](0053-code-worktrees-live-in-a-user-visible-root.md),
  [`0055`](0055-multiple-sessions-per-workspace.md),
  [`0059`](0059-workspace-reclaim-tiers.md),
  [`0062`](0062-pull-request-facts-and-attribution.md),
  [`0069`](0069-durable-code-session-queue.md)

## Context

A code workspace is one git worktree (record 32). Creating one runs
`git worktree add -b <branch> <path> <base>` and then the repository's setup
script, both inside `create_workspace`
(`crates/tidebreak-server/src/code/runtime.rs:1200`), and the call does not
return until the setup script does.

Record 55 opened that workspace to any number of interactive sessions and kept
the checkout safe by serializing turns on it. The mechanism is a per-workspace
async mutex, `Runtime::worktree_turn_lock`
(`crates/tidebreak-server/src/code/runtime.rs:5314`), minted on first use and
handed to every session's worker, so the lock outlives any one session and a
worker recovered after a restart rejoins the same queue. Taking the lock *is*
the reservation: no database read can stand in for it, because two idle
siblings both pass an "is a sibling running?" check before either one marks
itself running. The turn path acts on that in
`crates/tidebreak-server/src/code/session_worker.rs:1403-1428` — a user send
calls `try_lock` and fails fast with `WorktreeBusy`, while an already-queued
turn awaits the lock with control commands still answered, so a stop lands
before the turn starts rather than after.

The lock is not only the turn path's. Archive (`runtime.rs:1522`), the merge
preflight (`runtime.rs:2693`), and the transcript fork's settled-boundary check
(`runtime.rs:4774`) take it too, because each one reads or mutates the same
files.

What a reader sees today when they send a second turn: the message parks as a
durable `code_queued_turn` row and the composer's `QueueTray` shows it (record
69). They can edit it, reorder it, retract it, or send it now, and it runs when
the checkout frees. Nothing is dropped and nothing is silent — record 69 even
pauses the queue when a stop lands on a turn that is still waiting on a
sibling, because a sibling's turn ending releases the checkout but wakes
nobody.

What they cannot do is get two answers at once. A five-minute turn is a
five-minute wait for the second agent, whether or not the two agents would have
touched the same files. Sessions are cheap and people open several — a second
harness for a second opinion, a scratch conversation, a transcript forked into
fresh context. Record 55 promised that a second agent starts immediately. It
did not promise that it does anything immediately.

## The question

Should two turns ever run at the same time in one workspace, and if so, what
does the second one run against? The checkout is the only thing serialized;
sessions, queues, transcripts, and engine children above it are already
concurrent.

## Options

### A. Keep strict serialization and spend the effort on the wait

Leave the turn lock alone and make the queue explain itself: which session
holds the checkout and what it is doing, how long it has held it, where this
message sits in line, and a one-click offer to start the work in a new
workspace instead.

The tray shows a queued row today but not its reason. A row parked behind its
own session's turn and a row parked behind a sibling's turn look identical,
even though only the second one depends on a conversation the reader may not
have open. That gap is the complaint people actually voice; throughput is the
one they infer from it.

Cost: nothing structural. No migration, no wire change, no new failure mode.
Against it: two agents on one branch stay a sequence, and a long turn stays a
long wait.

### B. Optimistic parallelism in the one worktree

Let two turns run at once and rely on them touching different files.

Nothing bounds what a turn writes. The engine edits files as it goes, and there
is no manifest to intersect in advance, so "different files" is a hope, not a
precondition. Three concrete mechanisms break before the files do:

- **Checkpoints stop meaning what they say.** A checkpoint snapshots the whole
  worktree — tracked changes and untracked files alike — with `add -A` through
  a reusable index (`crates/tidebreak-server/src/code/checkpoint.rs:712-768`).
  A turn's checkpoint therefore captures every edit present in the tree,
  including a sibling's in-flight ones, so the per-turn diff
  (`checkpoint(n-1)..checkpoint(n)`) becomes "what changed in the tree while
  this turn ran". Record 32 sells that diff as what this turn changed, and
  review reads it that way.
- **The session baseline has nothing to anchor to.** Ordinal 0 records the
  worktree as it stood when a session was created, precisely so a sibling's
  earlier edits stay out of this session's turn 1 (`checkpoint.rs:20-23`). That
  boundary only exists because siblings take turns. Concurrently, there is no
  moment to record.
- **The index contends.** The snapshot index is one file per worktree, resolved
  by `git rev-parse --git-path tidebreak-checkpoint-index`
  (`checkpoint.rs:678`), and the code already recognizes
  `tidebreak-checkpoint-index.lock` and "another git process" as a failure it
  must recover from (`ReusableIndexFailure::Locked`, `checkpoint.rs:626`).
  Today that is rare crash residue. Under concurrency it is the ordinary case,
  at every turn boundary.

Add the shared build directory: two quick actions or two test runs in one
target tree do not conflict, they produce a wrong artifact.

Rejected as a default. It trades a bounded, visible wait for unbounded,
silent corruption that no surface can attribute afterwards — record 55's
"corruption, not concurrency", arrived at from the other direction.

### C. Fork on contention

A send that finds the checkout busy gets its own checkout, branched at the
holder's newest checkpoint, and the reader decides afterwards what becomes of
it.

The primitive exists. A checkpoint is an ordinary commit — `commit-tree`
against the previous checkpoint, published under a hidden ref
(`checkpoint.rs:426`) — so `git worktree add <path> <checkpoint-oid>` starts a
second tree that already contains the sibling's uncommitted work. Each tree
then gets its own checkpoint index, because `--git-path` resolves per worktree.

Three things are unsettled:

- **Merge back.** A fork that edits the files the parent turn is editing
  produces a conflict, and nobody is positioned to resolve it: the reader was
  not watching, and neither agent knows the other exists. Surfacing the fork
  unmerged is honest, but it changes the answer from "your second turn ran" to
  "your second turn ran somewhere else", which is a product decision, not an
  implementation detail.
- **Identity.** The workspace is what a branch, a diff, a pull request, and
  attribution key to. Record 62 stores one attribution row per
  `(pull request, workspace)`. An ephemeral checkout that pushes is either a
  workspace — in which case this is option D with extra steps — or its work is
  unattributable.
- **The word is taken.** Fork today means forking a *conversation*:
  `fork_transcript` (`runtime.rs:4752`) writes a condensed transcript plus a
  record per turn, and the reader hands it to a fresh agent in the same
  workspace. It never branches the checkout. Naming a checkout fork "fork" puts
  two meanings on one control.

Not rejected. It is the only option that buys real concurrency without giving
up the single-tree guarantee, and its cost lands somewhere the reader can see.
It needs merge-back and naming settled before it is buildable.

### D. A worktree per session

Every session gets its own checkout, so turns run in parallel with no lock.
Record 55 rejected this; the reasoning still holds, and the costs are now
measured.

- **Disk.** Record 59 measured a 1.1 GB checkout against a 12 KB bundle for the
  same branch in this repository, on a machine holding around eighty worktrees.
  The bytes are in the checkout. Multiplying checkouts by sessions multiplies
  exactly the expensive part.
- **Setup time.** `create_workspace` runs the repository's setup script inline
  and does not return until it finishes (`runtime.rs:1286`). A session-scoped
  worktree makes opening a second agent cost a full dependency install or
  build — handing back the one thing record 55 delivered.
- **Visibility.** Worktrees live in a user-visible root (record 53). One
  directory per conversation is a listing nobody asked for.
- **The original objection.** The branch, the diff, the pull request, and the
  review surface are all keyed to one tree. Splitting the tree means merging
  them back.

Rejected again for interactive sessions. A session that genuinely does not
share the goal wants a second workspace, and creating one is already the
answer.

## Recommendation

**Option A now, with option C as the shape to design toward.** This section
names a default so the argument has something to push against; the record's
status is `Proposed` and the owner accepts, amends, or rejects it. Merging is
the acceptance (see the [README](README.md)).

- Keep the turn lock exactly as record 55 defines it. Nothing here argues it is
  wrong, and every option except B keeps it.
- Make the wait legible in the tray: name the session holding the checkout and
  what it is running, show how long it has held it, and offer to start this
  message in a new workspace instead. The reader's real question is "why is
  this not running", and the answer already exists server-side.
- Do not build B. Its failure mode is a wrong diff, which is worse than a slow
  one because review cannot detect it.
- Revisit C once the queue is legible and there is evidence about what people
  actually wait on. If contention is mostly one long turn blocking short reads,
  C is worth its complexity; if it is mostly two agents editing the same code,
  the wait is the correct answer and C only moves the conflict later.

## Consequences

Accepting the recommendation changes no server contract: no migration, no wire
field, no new failure mode on the turn path. The work is desktop-side, on the
tray record 69 already shipped.

It also fixes code mode's answer to "two things at once" as "two workspaces",
which makes workspace creation load-bearing. Creation is synchronous and runs
the setup script inline, so the cost of that answer is whatever the repository's
setup script costs — worth measuring before leaning on it.

Choosing C later would put a second tree under one workspace, which record 32
excluded by name ("multiple worktrees per workspace"). That record would need an
amendment, not a silent reinterpretation.

What would reopen this: measurement showing queued turns routinely wait longer
than the turns they wait behind, a per-turn write manifest from any harness that
would make overlap decidable in advance, or a product decision that a second
agent's answer is worth delivering on a branch the reader has to reconcile by
hand.

## Validation

Option A is a client change, so its validation is behavioral: a queued row
parked behind a sibling names that sibling, and the name updates when the
checkout changes hands.

The wrong implementation to guard against computes the position in line from
the session's own queue alone. A message parked behind a sibling then reads
"1 of 1" and never moves, which is the exact confusion this option exists to
remove.
