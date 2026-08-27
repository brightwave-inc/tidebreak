# 78. Uneff me targets the Tidebreak product repo

- Status: Proposed
- Date: 2026-08-27
- Owners: code mode
- Related: [`0030-code-mode-separate-surface.md`](0030-code-mode-separate-surface.md),
  [`0032-code-workspaces-worktrees-checkpoints.md`](0032-code-workspaces-worktrees-checkpoints.md),
  [`crates/tidebreak-desktop/ui/src/code/uneffMe.ts`](../../crates/tidebreak-desktop/ui/src/code/uneffMe.ts)
- Supersedes: none

## Context

Uneff me is a dogfood command on a Code workspace session. It copies the
session debug dump into a *new* workspace whose job is to diagnose a Tidebreak
product bug and open a pull request against `main`. That only makes sense if
the new workspace is a checkout of Tidebreak itself, not the user's
application repo and not a Tidebreak *worktree* that Code already created
under `workspaces/tidebreak/…`.

The command already shipped in `#2762`. Identification lives in
`isTidebreakProductRepo` / `tidebreakProductRepo`: a connected repo counts as
the product checkout when its display name is `tidebreak` or
`brightwave-inc/tidebreak`, or when the checkout folder basename is
`tidebreak`. Worktree paths are not consulted as a match. When several
connected repos match, display name `tidebreak` wins, then basename
`tidebreak`, then the first remaining match. The command is hidden until that
repo is connected; starting it without one fails with "Add the Tidebreak
repository to Code first."

The open product call was whether that special case should stay, grow into
remote-URL detection or a settings flag, or become a generic "fix this
session's repo" action. This record freezes the behavior the code already
implements so later work does not invent a second surface.

## Decision

Uneff me is a Tidebreak-only dogfood path. It always creates the fix
workspace on the connected Tidebreak *product* repo, never on the source
session's repo and never on a Code worktree whose path happens to contain
`tidebreak`.

You identify that repo from Code's existing repo snapshot only: display name
and checkout folder basename. You do not probe `git remote`, GitHub
coordinates, or a user setting. You do not match on a worktree path.

If the product repo is not connected, you hide the command and refuse to
start. You do not prompt to add a repo, clone Tidebreak, or fall back to
another checkout.

The first prompt in the new workspace still tells the agent it is in the
Tidebreak source repository and should open a pull request against `main`.
That wording is part of the contract: Uneff me is not a generic debug-to-PR
shortcut for arbitrary repos.

## Alternatives Considered

### Match the git remote or GitHub repository id

Resolving `brightwave-inc/tidebreak` from `origin` would survive a renamed
folder. It was rejected because Code repo snapshots do not carry remotes
today, it would add a git round-trip to every palette render, and a wrong
remote on a coincidentally named folder is rarer than a false match on a
worktree path — which basename-only already avoids.

### A settings flag or "this is Tidebreak" checkbox

An explicit mark would remove name heuristics. It was rejected as a new
product surface for a dogfood command. You can add it later if forks or
unusual clone names become common; the heuristic is reversible.

### Uneff me against the source session's own repo

That would turn the command into a generic "file a fix from this debug dump"
action. It was rejected because the prompt, title prefix (`Uneff:`), and
failure copy all assume Tidebreak product work. A generic path is a different
feature.

### Do nothing (leave the heuristic undocumented)

The matching rules would keep working, but the next change would re-open
whether worktrees, remotes, or settings should participate. The record exists
so that argument stops.

## Consequences

A checkout whose folder is not `tidebreak` and whose display name is neither
`tidebreak` nor `brightwave-inc/tidebreak` is invisible to Uneff me until the
user renames the Code repo or reclones. Contributors who keep the product
under another directory name must set the display name.

False positives remain possible for an unrelated git repo whose folder is
literally `tidebreak`. That is accepted as cheaper than remote probing.

Revisit this if Code repo snapshots grow a stable remote identity, if Uneff me
is offered to people who do not have the product repo, or if the command is
generalized beyond Tidebreak.

## Validation

`uneffMe.test.ts` must keep covering: a `tidebreak` display name or basename
matches; a worktree under `workspaces/tidebreak/…` does not; display name
`tidebreak` beats a basename-only clone; `startUneffMeWorkspace` refuses when
no product repo is connected. A plausible wrong implementation that treated
any path containing `tidebreak` as the product repo would fail the worktree
case.
