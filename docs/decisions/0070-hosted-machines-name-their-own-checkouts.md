# 70. A hosted machine names its own checkouts and the caller's repositories

- Status: Proposed
- Date: 2026-08-24
- Owners: server, desktop
- Related: [`0053-code-worktrees-live-in-a-user-visible-root.md`](0053-code-worktrees-live-in-a-user-visible-root.md),
  [`0063-hosted-machines-borrow-forge-credentials.md`](0063-hosted-machines-borrow-forge-credentials.md),
  [`0065-hosted-git-acts-as-the-person.md`](0065-hosted-git-acts-as-the-person.md)

## Context

The add-repository dialog hides the destination field when
`chooses_destination` is true, which is exactly when `code_clone_parent_dir`
is stored. A hosted machine never writes that setting: no administrator has
set it, and no clone has succeeded yet, so a member who cannot see the
filesystem is asked to type a path on a machine they cannot browse.

Worktrees already solved the same problem. Decision 53 gives a headless
deployment `{data_dir}/code/worktrees` when no setting is stored, because
that data directory *is* the operator-visible location. Clones still have no
such default.

The repository field has the same shape of gap. Decision 65 already borrows
the caller's forge identity for clone, push, and pull requests. The dialog
still asks them to type `owner/repo` even though that identity can list the
repositories they can already clone. The gateway lists those names itself
(gateway ADR 0092) so the machine never holds a token for the read.

## Decision

1. **A self-host machine places clones itself.** When no
   `code_clone_parent_dir` is stored, the self-host profile uses
   `{data_dir}/code/src` and creates that directory on the first clone. The
   probe answers `chooses_destination: true`, so an attached window never
   asks for a path. A stored setting still wins. The desktop profile is
   unchanged: the first clone still names a destination, and that value is
   what later clones remember.

2. **An attached window that cannot name a destination says so.** If the
   window is not on the machine and the machine has no destination, the
   dialog shows that as an error with remediation rather than an empty
   text field. Local-folder registration is hidden in the same case: the
   caller cannot point at a path the machine would resolve.

3. **The GitHub form offers the caller's repositories.** On a
   gateway-authenticated hosted machine, `GET /code/repos/github` reads the
   gateway list for this caller and the dialog filters it as they type. A
   name that is not on the first page can still be typed as `owner/repo`.
   A failed list keeps that field and says the suggestions did not load.
   A machine with no lender keeps the typed field.

Deliberately excluded: paging past the gateway's first 100 names, listing
through `gh` on a desktop, and any change to where worktrees land.

## Alternatives Considered

- **Seed the setting from Helm or Terraform.** An env-var or apply-time
  write dies with the disposable store the next time the pod is recreated.
  A profile default survives every wipe. Rejected.
- **Ask the first clone to name a path, then remember it.** That is today's
  desktop rule. On a hosted machine the first caller cannot name a path.
  Rejected.
- **Mint a token to the machine and let it call `GET /user/repos`.** The
  mint wants a repository the picker does not have, and the read does not
  need the machine to hold a token. The gateway list is the same pattern
  as the probe. Rejected.

## Consequences

- A fresh hosted machine clones without asking where. Checkouts land under
  `{data_dir}/code/src/<owner-segment>/<name>`, beside worktrees, and go
  away when that disposable volume does.
- An operator who wants a different parent still sets `code_clone_parent_dir`.
- Revisit when a hosted volume should survive a reschedule, or when the
  first page of 100 names is not enough.

## Validation

- A self-host runtime with no stored setting answers
  `chooses_destination: true` and clones into `{data_dir}/code/src` without
  a `parent_dir`.
- A desktop runtime with no stored setting still answers false and still
  refuses an unaimed clone.
- The add-repository dialog hides Destination when the machine chooses,
  hides Local folder when the window is attached, and shows the caller's
  repositories on the GitHub form.
