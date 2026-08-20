# Working in Tidebreak

Guidance for coding agents. Humans start at [`CONTRIBUTING.md`](CONTRIBUTING.md).

Tidebreak is a Cargo workspace of Rust crates plus a Tauri desktop app whose UI
is React/TypeScript under `crates/tidebreak-desktop/ui`.

## Hard rules

- Do not use the `bw` CLI. Use this repository's Cargo, pnpm, and GitHub commands.
- Branch off `main`; PR back into `main`. Never commit straight to `main`.
- Commit or push only when asked. Do not merge unless the request was to merge.
- For parallel-track work, claim the GitHub issue (assignee + `in-progress`)
  before the first edit. Interactive work the user is directing here does not
  need an issue.
- Use GitHub REST (`gh` / `gh api`), not GraphQL. Shared agent sessions exhaust
  the GraphQL quota.
- Pin every direct dependency to an exact version that already matches the
  lockfile. Dependabot is the upgrade path.
- Add a test only if a failure would change what we do. Prefer contracts,
  easy-to-reverse decisions, bug reproductions, and end-to-end behavior.
- Run a cheap relevant subset locally; let CI do the rest. A conflicting PR
  ran nothing — rebase before trusting green.
- Enable auto-merge (`gh pr merge <n> --squash --auto --delete-branch`) so a
  ready PR lands when its lanes go green.
- This is a public repository. No secrets, no private design docs.

## GitHub tracking

The [Tidebreak GitHub Project](https://github.com/orgs/brightwave-inc/projects/5)
is the board. Milestones are delivery buckets. Parked product ideas stay in
[`docs/deferred.md`](docs/deferred.md) until a slice is buildable; do not copy
that list onto the board.

- Open an issue only for a bounded outcome with an owner. Do not file per-file
  or per-session tasks.
- Multi-slice work uses an `epic` parent. Children are the slices, not the
  files inside them.

## Desktop UI workflow

- When adding or changing a reusable visual component or a meaningful UI state,
  add or update its Storybook story under
  `crates/tidebreak-desktop/ui/src/stories/`.
- Show the states that matter for evaluation, including loading, empty, success,
  failure, and compact variants when they apply. Reuse the typed Storybook
  fixtures instead of duplicating ad hoc sample data.
- Run `scripts/storybook.sh` while developing UI, and run
  `pnpm --dir crates/tidebreak-desktop/ui storybook:build` before publishing
  relevant UI changes.

## Pointers

- Humans and commit/PR titles: [`CONTRIBUTING.md`](CONTRIBUTING.md)
- Board: [Tidebreak GitHub Project](https://github.com/orgs/brightwave-inc/projects/5)
- Decisions: [`docs/decisions/`](docs/decisions)
- Parked scope: [`docs/deferred.md`](docs/deferred.md)
- Releases: [`docs/releases.md`](docs/releases.md)
- Cross-provider replay: [`docs/model-providers.md`](docs/model-providers.md)
