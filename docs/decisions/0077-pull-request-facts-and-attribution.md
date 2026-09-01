# 77. Pull Request Facts And Attribution

- Status: Proposed
- Date: 2026-08-22
- Owners: code mode
- Related: [`0042-user-initiated-pr-merge.md`](0042-user-initiated-pr-merge.md),
  [`0050-watch-and-fix-is-a-durable-task.md`](0050-watch-and-fix-is-a-durable-task.md),
  [`0052-harness-subagents-as-child-rows.md`](0052-harness-subagents-as-child-rows.md),
  [`0055-multiple-sessions-per-workspace.md`](0055-multiple-sessions-per-workspace.md),
  [`0060-triggers-are-durable-rules-on-pull-request-facts.md`](0060-triggers-are-durable-rules-on-pull-request-facts.md),
  [`0061-schema-changes-are-migrations.md`](0061-schema-changes-are-migrations.md)

## Context

A workspace produces pull requests over its whole life, sometimes several,
sometimes in repositories other than its checkout. Nothing durable records
them. `code_workspace.pr` is a single JSON digest with no repository
identity — a bare number, last write wins, never cleared — read back from
`gh pr view` with the worktree as working directory. The delivery surface
(`crates/tidebreak-server/src/code/delivery.rs`) reads pull requests across
repositories with full identity, but holds them only in a 30-second
in-memory cache, and computes the pull-request↔workspace link per request
with a three-tier heuristic (`links_for_pr`): stored digest number, head
SHA, then a branch-name guess. Restart the app and every cross-repository
observation is gone; ask "what did this workspace ship" and the answer is a
guess recomputed from whatever GitHub returns this minute.

Meanwhile the acts themselves are already journaled. Every shell command an
agent runs lands durably in `code_event` as
`ToolDetail::Command { cmd, cwd }`, tagged with `parent_call_id` when a
harness subagent ran it (record 52), with the tool result's bounded preview
beside it. `tidebreak-shell-policy` ships `simple_command_argvs`, a
fail-closed parser from a compound command line to argv lists. Nothing reads
any of it semantically.

One more force: the user runs review and triage agents that read, comment
on, and merge other people's pull requests in bulk. Any definition of "this
workspace worked on that pull request" that counts those interactions would
drown the signal.

## Decision

**A `code_pull_request` row is a durable, confirmed observation of one pull
request.** Identity is `(owner, host, repo_owner, repo_name, number)`, so a
pull request in a repository with no local checkout is representable. The
snapshot carries url, title, coarse state (open/merged/closed), draft,
author, head and base branch, head SHA, and the host's created/updated/
merged/closed times, plus local `first_seen_at` and `last_seen_at`. GitHub
stays authoritative: rows record what was observed and when, never what
Tidebreak wishes were true. Checks, review decisions, and mergeability are
deliberately excluded — they stay live-only in the delivery reads, where a
20–30 second cache is the right freshness.

**Attribution comes from confirmed repository changes.** `gh pr create` makes
a workspace the pull request's author (`authored`); `git push` whose branch is
or becomes a pull request's head makes it a contributor (`contributed`). The
post-turn detector also recovers a contributed tie from the workspace
checkout when the turn checkpoint changed files, the checkout is clean, and
it remains on the workspace branch with a current commit that exactly matches
an open pull request head. This fallback catches create and push commands that
fell outside the bounded journal tail, and pull requests opened through
another GitHub client. Views, checkouts, comments, closes, and merges alone
never mint attribution, so a review agent that triages thirty pull requests
claims none of them. One row per
`(pull_request, workspace)` holds the strongest claim; a contributed row
upgrades to authored when authoring evidence appears. Attribution rows carry
the session and, when the act ran inside a subagent's `Task` span, its
`parent_call_id` — so "which agent opened this" is answerable down to the
child row.

**Local evidence is a hint, never the fact.** A post-turn detector reads the
closed turn's journaled commands, recognizes create and push through
`simple_command_argvs` (which fails closed on substitutions and parse
errors), and then confirms each candidate against the host with one
repository-qualified `gh` read before writing anything. If the command tail
does not carry either act, the detector can inspect the turn checkpoint and
workspace checkout under the exact clean-checkout and matching-head rules
above. A command whose completion the engine reported as failed is never
confirmed — the read could match an older pull request on the same branch and
mis-attribute it. The detector is best-effort and bounded (journal tail,
parse count, and confirm count are capped per turn); it never fails the turn,
and it journals no new event kind. The user-initiated create and push routes
mint through the same confirm-then-record helper.

**The reconcile sweep owns freshness and the misses.** A later slice adds a
sweep that re-reads tracked repositories, updates snapshots, and mints
`contributed` attribution for the delivery heuristic's exact tiers only
(digest number, head SHA). The branch-name tier never mints: it is a
display-time guess and stays one. Auxiliary terminals are not journaled
(record 36), and a fork workflow — push to a fork's remote, pull request on
the upstream — escapes the head-branch confirm; both are corrected by the
sweep when the pull request is in a tracked repository, and accepted as
gaps otherwise.

**Relation to record 60.** Trigger conditions keep classifying the live
digest for state (`checks_failed`, `behind`, …). What facts add is the edge
the digest cannot see: a pull request coming into existence, and a head
moving between sweeps, with `first_seen_at` as the anchor. Those become
trigger conditions in a later slice; this record only lays the substrate
they read.

Deliberately excluded: historical journal backfill (old sessions' commands
are not scanned; the sweep seeds current state); webhooks; a
`gh search prs --author @me` net, which cannot distinguish an agent's act
from the user's own unrelated pull requests and would mint noise the
attribution rule exists to prevent; and any write to GitHub — this substrate
only observes, and record 42's merge boundary is untouched.

## Alternatives Considered

**Do nothing; keep recomputing links per request.** Rejected: the heuristic
cannot answer "every pull request this workspace worked on" for anything
older than the current branch state, loses cross-repository work entirely,
and forgets everything on restart.

**Widen `code_workspace.pr` to a list.** Cheapest schema change. Rejected:
the digest has no repository identity, so cross-repository rows are
unrepresentable; every consumer of the single slot would need auditing for
list semantics; and the workspace row is the wrong owner for a fact that
can be shared by several workspaces.

**Intercept HTTP to detect pull-request writes.** The gateway does this for
its own sandboxes, and it is the cleanest signal where traffic is brokered.
Rejected here: code-mode harnesses run with unmediated network by design,
Tidebreak's proxies are CONNECT-only and see host:port at most, and GitHub
access goes through the local `gh` binary. The journal already records the
act at process level; parsing it costs nothing new at runtime.

**Mint attribution from transcript evidence alone, without a confirming
read.** Fewer GitHub calls. Rejected: transcripts contain URLs and commands
that were read, quoted, or failed; a row minted from text alone is a guess
wearing a fact's clothes. The confirm read is the line between the two.

**Count every pull-request interaction as attribution.** Simpler rule.
Rejected for the review-agent force above: stewardship is not authorship,
and the bird's-eye view is only useful if it shows what a workspace
produced.

## Consequences

- Two new tables and three nullable `code_repo` columns, appended as
  migration `m20260822_000009_code_pull_request_facts` per record 61.
- Every turn end runs a bounded journal scan. A turn with no create, push, or
  changed checkpoint costs a few hundred event deserializations and no GitHub
  read. A changed, clean checkout can add one read. Turns remain capped at
  four confirming reads.
- Attribution survives workspace archive and release (plain foreign keys to
  soft-removed rows), so the bird's-eye view keeps history the workspace
  list no longer shows.
- `discovered_via` distinguishes post-turn confirmation (`command`) from
  sweep matching (`reconcile`); the user-initiated routes record `command`,
  since they confirm the same way.
- A pull request opened from an auxiliary terminal in an untracked
  repository is invisible until something else makes the repository
  tracked. Revisit if that gap turns out to be common — the hook would be a
  bounded author-scoped search, weighed against the noise it reintroduces.
- Revisit the two-act rule if a workflow appears where comment-driven work
  (for example, a review agent that only ever comments) needs to surface in
  the same view; that is a different relation, not a wider `contributed`.

## Validation

- A journaled `gh pr create` whose confirm read succeeds mints one fact and
  one `authored` attribution; re-running the detector on the same turn
  mints nothing new (the claim insert is idempotent).
- A journaled `git push` to a branch with an open pull request mints
  `contributed`; the same push when the branch has no pull request mints
  nothing.
- A turn that changed the workspace checkout mints `contributed` when the
  checkout is clean, remains on the workspace branch, and its current commit
  exactly matches an open pull request head. Another branch, a different head,
  or uncommitted work mints nothing.
- A `gh pr view`, `gh pr comment`, or `gh pr merge` in the journal mints
  nothing — the case a wrong implementation most plausibly passes, since
  the strings look similar.
- A failed `gh pr create` (non-zero exit) mints nothing even when an older
  pull request exists on the same branch.
- A signed-out or absent `gh` mints nothing and the turn still completes.
- An upsert against an existing row updates the snapshot and
  `last_seen_at` but never moves `first_seen_at` — the anchor a wrong
  implementation would silently reset on every sweep.
