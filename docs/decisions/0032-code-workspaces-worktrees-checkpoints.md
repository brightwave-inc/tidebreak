# 32. Workspaces: Worktrees, Branches, and Per-Turn Checkpoints via Git Shell-Out

- Status: Accepted (amended 2026-09-23, see [checkpoint restore](#amended-2026-09-23-checkpoint-restore))
- Date: 2026-08-15
- Owners: code mode
- Related: [`0030-code-mode-separate-surface.md`](0030-code-mode-separate-surface.md),
  [`0035-code-mode-wire-contract.md`](0035-code-mode-wire-contract.md),
  [`docs/code-mode.md`](../code-mode.md)

## Context

A code-mode workspace must give a coding agent an isolated place to work on a
real repository: the user's checkout must never be disturbed, several
workspaces must coexist on one repo, and the result must be an ordinary branch
the user can review, push, and merge. Git worktrees are the native mechanism
for exactly this.

Tidebreak has no git integration today — no library dependency, no shell-out,
no repo model. Whatever this record chooses is the foundation everything else
(diffs, review, the PR flow) stands on.

Review needs more than "the diff so far": a session that runs for ten turns
needs *turn-scoped* diffs — what did this turn change — which requires
recording the worktree state at each turn boundary without polluting the
branch history the user will eventually open as a pull request.

Two operational realities constrain the design. First, users' git
environments are configured: credential helpers, `core.hooksPath`, sparse
checkout, LFS, custom drivers. An embedded git implementation sees none of
that configuration the way the user's own `git` binary does. Second, the
server process can crash or be killed while harness children are alive;
recovery must never destroy user work.

## Decision

**Git runs as the user's own `git` binary via shell-out.** Tidebreak takes no
git library dependency. Every git operation is a bounded, non-interactive
subprocess (`GIT_TERMINAL_PROMPT=0`, explicit timeouts, captured output) with
arguments built as an argv array, never a shell string.

**Repos.** Registering a repo validates it with
`git rev-parse --show-toplevel`, canonicalizes to the toplevel, and refuses
bare repositories and nested registrations of the same toplevel. A repo
carries: display name, default base ref, branch prefix (default
`tidebreak/`), optional setup and archive scripts, and quick actions (named
commands the user can run in a workspace).

**Workspaces.** Creating a workspace creates a worktree and branch in one
step: `git worktree add -b <branch> <path> <base>`, with the worktree rooted
under the Tidebreak data directory at
`code/worktrees/<repo-slug>/<workspace-slug>/` — never inside the user's
repository. Branch names are the repo's prefix plus a slug of the workspace
title, with generated two-word fallback names when untitled; a branch
collision is a user-visible error, not an auto-suffix. After creation the
worktree is verified (`git rev-parse --is-inside-work-tree`); a half-created
worktree is removed and the creation fails cleanly. The setup script, if any,
then runs inside the worktree; a setup failure preserves the checkout, marks
the workspace `SetupFailed`, and requires an explicit user choice to continue
anyway or destroy.

**Checkpoints.** Every completed turn records the worktree's full state —
tracked changes and untracked files — as a synthetic commit created through a
temporary index file, referenced by a hidden ref
`refs/tidebreak/checkpoints/<workspace>/<turn>`. The user's index, `HEAD`,
and reflog are untouched; no visible commit appears on the branch. Checkpoints
give three read paths: the diff of one turn
(`checkpoint(n-1)..checkpoint(n)`), the workspace diff against base
(`merge-base(base, HEAD)..worktree`, including uncommitted state), and a
future restore path (out of scope here; the refs make it possible). Diffs are
produced server-side and bounded — capped in bytes and file count, truncation
explicitly marked — and the renderer never runs git.

**Archive.** Archiving a workspace runs the archive script (same
failure-preserves rule as setup), checks for uncommitted or unpushed work and
requires an explicit `force` to discard any, then
`git worktree remove` (tolerating already-gone), `git worktree prune`, and
checkpoint-ref cleanup. The branch is kept unless the user asked to delete
it.

**Crash recovery is conservative.** Session rows persist the harness child
pid and a per-spawn epoch. On boot, a session recorded as running is probed:
child dead → the open turn is closed as interrupted (journaled) and the
session is idle; child alive → the session is **fenced** — observed but not
controlled — and only an explicit user reap resolves it. Processes are never
killed by name or pattern, only by a pid recorded at spawn; `EPERM` from a
signal-0 probe counts as alive; any pid-reuse doubt fences rather than kills.

Deliberately excluded: checkpoint restore UX, multiple worktrees per
workspace, and any git operation on the user's primary checkout beyond
read-only queries against the registered repo.

## Alternatives Considered

**A git library (`git2`/`gix`).** Rejected: heavy exact-pinned dependencies,
and — decisive — an embedded implementation does not see the user's git
configuration (credential helpers, LFS, sparse settings) the way their own
binary does. Shell-out also keeps every git operation inspectable in logs as
a plain command.

**Worktrees inside the user's repository** (e.g. `<repo>/.tidebreak/`).
Rejected: pollutes the user's tree, their ignore files, and every tool that
walks the repo. Data-dir placement keeps ownership and cleanup with Tidebreak.
Known cost, accepted: some toolchains resolve paths relative to repo
ancestry and behave differently outside it; a per-repo location override is
deferred until that pain is real.

**Visible commits per turn.** Rejected: pollutes the branch history the user
will open as a PR, and rewriting it away (squash on archive) is exactly the
kind of history mutation that destroys user trust when it goes wrong. Hidden
refs record the same information out of band.

**Snapshot by copying files** instead of git checkpoints. Rejected: quadratic
in workspace size, loses rename/mode fidelity, and reimplements what git's
object store already does content-addressed.

**Kill orphaned harness children on boot.** Rejected: pid reuse makes it
unsafe, and a wrongly killed process may take hours of agent work with it. A
fenced card the user resolves is strictly better than a silent kill.

**Do nothing** (run sessions in the user's checkout). Rejected: a single
agent mistake contaminates the user's working state, and parallel sessions
are impossible.

## Consequences

Tidebreak becomes a git citizen: it must behave well when the user's git
config is unusual, when `git` is old, and when a repo is large. Version and
capability checks happen at repo registration, not mid-session.

Archive cleans a workspace's refs. Refs orphaned by crashes may remain because
separate Tidebreak profiles can share one repository, and one profile cannot
safely decide that another profile's ref is unused. Checkpoint commits share
the repository's object store, so their cost is incremental.

The data-dir worktree location makes "where is my code?" a product question;
the workspace UI must surface the path prominently (open in editor / reveal /
copy path).

Revisit this decision if per-turn checkpoints prove too slow on large repos
(would argue for making checkpoints asynchronous or opt-out per repo), or if
the data-dir location breaks enough real toolchains to justify the per-repo
override now.

## Validation

Integration tests against throwaway temporary repositories:

- create → verify → setup-script failure preserves the checkout and state;
- checkpoint after a turn with tracked edits, untracked files, renames, and
  mode changes; turn diff and workspace diff both correct and bounded, with
  truncation marked when caps are exceeded;
- the user's index and `HEAD` are byte-identical before and after a
  checkpoint;
- archive with uncommitted work refuses without `force`; with `force` it
  removes worktree and refs and keeps the branch;
- a half-created worktree (simulated failure between `worktree add` and
  verification) is cleaned up;
- already-removed worktrees archive without error (prune path);
- boot recovery: dead pid closes the open turn as interrupted; a live decoy
  process with the recorded pid fences and is never signaled; `EPERM` probes
  count as alive.

A plausible wrong implementation checkpoints only tracked files and passes
every tracked-edit test; the untracked-file case above must fail it. Another
passes recovery tests by killing the decoy; the never-signaled assertion
(decoy still alive after boot) must fail it.

## Amended 2026-09-23: checkpoint restore

The restore path this record deferred ships for 1.0. Reverting a file or one
hunk from a diff, and discarding a file's uncommitted changes, ship beside it
and share its rules; [`docs/code-mode.md`](../code-mode.md#undo-in-the-worktree)
carries the mechanics. One invariant governs all three: an undo never
overwrites or removes anything the person did not pick, and everything a
restore replaces can be brought back by its Undo.

**What a restore targets.** "Before a turn" is the `from` of that turn's own
diff: the previous checkpoint in the session's chain, the start baseline for
turn 1, or where the chain resumed after an earlier restore. A turn with no
such checkpoint is refused. The merge base the diff falls back to knows
nothing of the untracked files that stood in the worktree then, so restoring
to it would delete them, and an older checkpoint would also undo turns the
person did not pick. In a workspace several sessions share, that state
predates other sessions' later turns too; the confirmation names those turns
rather than let "before this turn" hide them.

**What a restore changes.** Every file the checkpoint snapshot sees goes back:
tracked and untracked, added, modified, deleted, and renamed. `HEAD`, the
branch, the reflog, and the user's index are untouched. A turn that committed
leaves its commit on the branch, and the restore shows as uncommitted changes
that undo it. Rewriting the branch was rejected above for the same reason it
was rejected here: history mutation is the change that destroys trust when it
goes wrong.

**How files move.** Tidebreak moves the files itself, one path at a time,
while it holds the worktree lock. It runs no `git checkout`, `read-tree`, or
`checkout-index`: those check every path once, up front, then overwrite an
ignored file that appears after the check, and a checkout they stop partway
does not say which paths it wrote. Tidebreak takes a fresh snapshot through
a private index, lists the paths that differ from the target, and records
what the worktree holds at each. Right before it touches a path, it checks
that the path still holds what it recorded, or is still absent, and stops
rather than overwrite or remove anything that changed since. It removes
paths first, deepest first, then writes, parents first. It writes each file
to a temporary file beside it, checks the path one last time, and moves the
file into place with a rename, or with a hard link where nothing stood. A
path holds its old content or its new content, never a mix, and a file that
appears meanwhile is never overwritten. When it stops, every path it moved
goes back, and it checks each one against the saved state. No checkout hook
fires, and a file whose content matches is never touched.

**What a restore refuses.** Ignored files are in no snapshot, so no Undo could
bring one back. A restore that would overwrite or remove one, or a folder
holding one, or any other file no snapshot holds, refuses and names it. So
does one that would remove or replace a nested repository, a submodule, or a
gitlink, or a folder that holds one: a snapshot holds only its commit, never
its files. The preview names the same paths, so the person can move them
first. A sparse checkout refuses every undo, because its snapshot cannot
tell a file outside the cone from a deleted one.

**A restore can be undone.** Before any file moves, the state being replaced
is committed to its own hidden ref,
`refs/tidebreak/checkpoints/<workspace>/<session>/restore/<id>`, and the
restore is journaled as started. Undo is a restore whose target is that
state, and it saves its own. The confirmation lists every change since the
target, whoever made it, and the restore takes the preview's snapshot tree
back as `expected_tree`: a worktree that moved since the person confirmed is
left alone. When a restore stops partway and every path it moved checks out
against the saved state, it is journaled as failed, the one outcome that
says nothing changed. Otherwise it is journaled as partial. Its Undo,
reachable from the transcript and by id, puts back everything it replaced,
and its row keeps the Undo whatever the status, including a restore the
process never finished. The route runs the restore on a task of its own, so
a client that disconnects cannot stop it halfway.

**The chain continues from the restore.** For every open session in the
workspace, `…/<session>/after/<n>` points at the restored state, where `n` is
that session's newest turn. The next turn diffs from it. Without that, the
next turn's diff would start at its own previous checkpoint and claim the
restore's reversal as the turn's work. The same ref tells the engine, ahead of
the next message, which files moved since its last turn. Its own memory of
the undone turns stays, and [`docs/deferred.md`](../deferred.md) carries
rewinding it.

**It runs between turns only.** The worktree turn lock is tried, not waited
for, and a held lock is a refusal (`turn_running`). A session fenced for an
engine that may still be alive in the checkout refuses it too (record 55).
A sandbox workspace refuses it (`workspace_remote`). A commit follows the
same rule now: it refuses at once while a turn runs or a message waits in the
queue, and it carries only the tree the person reviewed, so it never commits
a turn's work under the person's message.

**Reverts are exact.** Diffs pair a renamed file with its old path, so a
rename never reads as an added file. A hunk is undone at exactly the lines it
names on the file the diff shows, then carried onto the current file with a
three-way merge; an overlapping later edit refuses the revert instead of
landing somewhere that reads the same. A discard acts on exactly the paths
its row names, both of a renamed file's, and leaves alone one with nothing
uncommitted. It never removes a folder nobody named. Reverts and discards
move files the way a restore does, and refuse the same paths.

**It is journaled.** `CheckpointRestored` lands in the transcript of the
session that owns the target, naming the restore, its target, what it
changes, and how far it got. A restore belongs to no turn and writes no turn
row.

**It is reachable headless.** `POST /code/workspaces/{id}/checkpoints/restore`
serves the desktop and `tidebreak code restore` alike (record 7). The restore
stays a user action: no agent tool and no automation path restores a
workspace.

Still excluded: rewinding the engine's own conversation with the files, which
[`docs/deferred.md`](../deferred.md) carries, and restoring a sandbox
workspace from the desktop.

Validation: `crates/tidebreak-server/src/code/checkpoint/restore.rs`,
`revert.rs`, and `worktree.rs` run against throwaway repositories. A restore
brings back tracked and untracked files, removes added ones, leaves ignored
files, `HEAD`, and the index byte-identical, rewrites no unchanged file, and
is undone by restoring its saved state. It refuses a worktree that moved
after its preview, an ignored file or folder in its way, a nested repository
in its way, a file or an ignored file that appears between its check and its
write, and a missing checkpoint, rather than restore further back. A restore
that stops partway, at a folder that refuses a removal too, is rolled back
and says so. A plan stopped after any one path leaves every path whole, in
its state before or after, and restoring the saved state brings back the
before state. A hunk of a renamed file reverts under the new name, a stale
hunk refuses rather than land on an identical block, and a discard refuses a
folder in its way and touches exactly the paths it is given.
`a_turn_after_a_restore_diffs_from_the_restored_state` in `checkpoint.rs`
pins the chain. `tests/code_undo.rs` in `tidebreak-server-api` drives the
routes end to end: the journal rows, the note to the engine, the refusals
while a turn is parked, commit included, and a commit over unreviewed
changes. A plausible wrong implementation restores tracked files only and
passes every tracked-edit case; the untracked file that must come back fails
it. Another checks out with `--reset` and passes every case without ignored
files; the ignored `.env` that must survive fails it. A third applies the
plan with `read-tree -m -u` and passes whenever nothing races it; the
ignored file that appears after the check fails it.
