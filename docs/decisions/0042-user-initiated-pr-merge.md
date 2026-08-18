# 42. User-Initiated PR Merge Through a Dedicated Endpoint

- Status: Accepted
- Date: 2026-08-17
- Owners: code mode, git/gh integration
- Related: [`0032-code-workspaces-worktrees-checkpoints.md`](0032-code-workspaces-worktrees-checkpoints.md),
  [`0035-code-mode-wire-contract.md`](0035-code-mode-wire-contract.md)

## Context

Tidebreak's `gh` runner (`crates/tidebreak-server/src/code/gh.rs`) refused any
argv containing `merge`, `--merge`, `--auto`, or `graphql`, so nothing in the
app — agent, automation, or user — could merge a pull request. That total ban
was deliberate: PR creation and status reads run on behalf of agent-driven
flows, and an agent must never be able to land its own work.

The desktop PR card now needs a merge button and an auto-merge toggle. Merging
is a user decision, not an agent capability, so the question is where the
ability to run `gh pr merge` may live without weakening the agent-path
refusal.

## Decision

Merging happens only through `POST /code/workspaces/{id}/pr/merge`, a
user-initiated endpoint. Its handler is the single caller of
`merge_pull_request`, which drives `run_gh_user_merge` — a runner that
executes only `pr merge …` argv and refuses everything else.

The general runner (`run_gh`), used by every creation, status, and comment
path, keeps its hard refusal of `merge`, `--merge`, and `--auto`. The
`graphql` refusal stays absolute on both runners: no Tidebreak path runs
GraphQL `gh` commands.

Excluded on purpose: no agent-callable merge tool, no auto-merge armed by any
background or automation flow, and no configuration that widens the merge
runner beyond `pr merge`.

## Alternatives Considered

- **Keep the total ban.** Safest, but it pushes the user to a terminal for the
  last step of a flow the app otherwise carries end to end, and the ban's
  purpose was to constrain agents, not users.
- **Let agents merge behind an approval prompt.** Rejected: merging publishes
  reviewed work to a shared branch, and an approval card is too easy to
  confirm reflexively. The blast radius of a wrong merge is a shared `main`,
  not a local worktree.
- **One runner with a boolean "allow merge" flag.** Rejected: a flag on the
  shared runner spreads through call sites and one mistaken `true` silently
  re-arms merging on an agent path. A separate, narrowly named function makes
  misuse visible in review and pinnable in tests.

## Consequences

Every future `gh` capability must choose a runner, which keeps the
merge boundary explicit. The merge endpoint maps host refusals to a
structured `pr_not_mergeable` error so the UI can render them.

Revisit if merging ever needs to ride an automation flow (for example,
"auto-merge when checks pass" driven by Tidebreak rather than by GitHub's own
auto-merge), or if the harness boundary gains a first-party review/merge
delegation with its own authorization story.

## Validation

`code::gh` tests pin both sides: the creation/status test asserts the general
runner refuses merge argv before spawning, and the merge-runner test asserts
it refuses everything except `pr merge` (GraphQL included) while the dedicated
operation runs `pr merge --squash` / `--merge --auto`. A wrong implementation
that merged through the general runner, or ran arbitrary commands through the
merge runner, fails those tests.
