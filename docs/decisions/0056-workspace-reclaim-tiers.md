# 56. Workspace Reclaim Tiers

- Status: Accepted
- Date: 2026-08-21
- Owners: Code mode
- Related: [`0032`](0032-code-workspaces-worktrees-checkpoints.md),
  [`0002`](0002-pre-v1-schema-and-persisted-format-mutability.md),
  [`0053`](0053-code-worktrees-live-in-a-user-visible-root.md)

## Context

Archive removed a workspace's worktree and kept its branch, so `restore` was
`git worktree add` and the transcript was never touched. That freed the
expensive part — a checkout carrying build output and dependencies — and left
the branch, its objects, and any clone behind.

For someone running many parallel agents that residue accumulates. The
machine this record was written on holds around eighty worktrees. Archiving
aggressively is the answer, but only if archiving is not a decision about
whether the work is still reachable.

The measurement that shapes the design: on a real branch in this repository,
`git bundle create main..branch` is 12 KB against a 1.1 GB checkout. The bytes
are in the checkout, not the commits. So the branch can be dropped and still
restored, for storage that rounds to nothing.

## Decision

**A workspace has reclaim tiers, and the transcript is outside them.**

| Tier | On disk | Rebuild |
| --- | --- | --- |
| Active | worktree | — |
| Archived | branch only | `git worktree add` |
| Released | nothing | unbundle, then `git worktree add` |

**Release bundles `base..branch`, not the whole history.** The base is still
in the repository; carrying it would scale the bundle with the project instead
of with the work. Bundles live at `<data_dir>/code/bundles/<id>.bundle`.
Unlike a worktree (record 53) a bundle is derived data, not user work, so it
belongs in the app-data directory beside the database that records it.

**The bundle is written and measured before the ref is dropped.** A failure
anywhere earlier leaves an archived workspace with its branch — exactly where
it was.

**Each tier confirms what it makes unrecoverable, and confirmation is not a
correctness gate.** Archive already refuses uncommitted or unpushed work
without `force`. Release refuses a branch holding commits the base does not,
with `branch_unmerged`. The bundle means the answer is recoverable either
way; the prompt exists because dropping a ref should be deliberate.

**Restore is one path.** A released workspace fetches its branch back from
the bundle and then follows the archived path unchanged, including the setup
script. Restoring clears the release bookkeeping and removes the bundle,
which is by then a second copy of commits git already has.

**Row and journal outlive the bytes at every tier.** Nothing in a reclaim tier
deletes a session, turn, or event. This is what makes the tiers safe to use
rather than a decision to lose work.

Deliberately excluded: reclaiming a clone directory, which recursively deletes
a directory the user may have registered themselves and needs its own
confirmation; a size guard on unusually large bundles, which wants a threshold
real use should set; and searching transcripts across put-away workspaces
(#2440), which is what makes deep reclaim comfortable rather than possible.

## Alternatives Considered

**Delete the branch with no bundle.** What Orca does on cleanup. Simplest, and
it reclaims the same bytes — but the work is then gone, so the confirmation
has to carry weight the user may not be able to judge. The bundle costs
kilobytes and removes the stake from the decision. Rejected.

**Bundle the full history rather than `base..branch`.** Self-contained, and
restorable into a repository that no longer has the base. Rejected: it scales
with the project, which is the cost this tier exists to avoid, and the base
being present is the premise of the workspace already.

**Store bundles as blobs.** The blob subsystem has leases, reference counting,
and a retirement queue built for chat attachments. Using it would couple code
mode to the chat content model ahead of record 48 step 5, for a file whose
lifetime is exactly one row's. Rejected.

**One tier: make archive drop the branch.** Fewer states. Rejected: archive's
cheap restore is what makes it a routine action, and folding an irreversible
step into it would make people hesitate over the one they should reach for
most.

## Consequences

- Deep reclaim is now a normal action rather than a cleanup project. The tier
  a workspace sits in describes what is on disk, not how finished the work is.
- Every surface asking "is this still live?" must ask about both put-away
  tiers. The UI does this through one `isPutAway` helper rather than comparing
  to `"archived"`, which is the mistake a new tier invites.
- Baseline edit plus an epoch bump, per record 2.
- A released workspace depends on a file outside the database. Losing the data
  directory loses the bundle, and restore fails with the branch still absent
  rather than half-populating the object store.

## Validation

The round trip is the claim: a released branch is deleted, restored from its
bundle, and the file its commit added is back on disk with the same tip. A
wrong implementation that bundles nothing, or fetches without verifying, fails
there rather than in review.

A separate test asserts the bundle stays under 100 KB for a one-line commit on
a base carrying a 400 KB blob. That is the efficiency premise stated as a
test: if a future change bundles full history, the tier stops being worth its
step and this fails.

The case a plausible wrong implementation still passes: dropping the branch
inside the same step that writes the bundle looks correct until the write
fails. The ordering is asserted by the refused-release test, which checks the
branch still exists after `branch_unmerged`.
