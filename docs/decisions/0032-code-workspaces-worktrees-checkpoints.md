# 32. Workspaces: Worktrees, Branches, and Per-Turn Checkpoints via Git Shell-Out

- Status: Proposed
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

Hidden refs accumulate; archive cleans a workspace's refs, and a periodic
sweep covers refs orphaned by crashes. Checkpoint commits share the repo's
object store, so their cost is incremental.

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
