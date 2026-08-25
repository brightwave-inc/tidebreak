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

- Read [`crates/tidebreak-desktop/ui/DESIGN.md`](crates/tidebreak-desktop/ui/DESIGN.md)
  before visual work. It defines the palette, type scale, status vocabulary,
  and the canonical patterns for settings, cards, approvals, empty states, and
  icons; `src/stylesContract.test.ts` enforces the mechanical rules, so
  arbitrary font sizes and raw Tailwind palette classes fail CI.
- When adding or changing a reusable visual component or a meaningful UI state,
  add or update its Storybook story under
  `crates/tidebreak-desktop/ui/src/stories/`.
- Show the states that matter for evaluation, including loading, empty, success,
  failure, and compact variants when they apply. Reuse the typed Storybook
  fixtures instead of duplicating ad hoc sample data.
- Run `scripts/storybook.sh` while developing UI, and run
  `pnpm --dir crates/tidebreak-desktop/ui storybook:build` before publishing
  relevant UI changes.

## Design-system approach

Tidebreak treats the design system as the product's memory, not just a
component library. Tokens and primitives are necessary but not sufficient;
the system must also carry the recurring judgment calls that make a screen
feel like Tidebreak.

- Classify every surface before building it. The four surfaces — Orienting,
  Index, Bulk edit, Resource detail — decide density, interaction model, and
  how much chrome is allowed. A surface is not a route; a settings page can
  host an Index rail next to a Resource detail panel.
- Use the canonical pattern for the surface. `SettingsPanel` owns the page
  shape. `ToolCardShell` owns the expandable tool row. `ApprovalCard` owns
  consent. `ChatStatusChip` owns activity summary. Do not invent a second
  answer to a question the system already settled.
- Keep icons subordinate. Lucide icons are lightweight recognition markers,
  not decorative advertising. Size them to the surrounding text, align them
  to the primary text line, and never place them in a gray container on
  high-density surfaces. `EmptyMedia variant="icon"` draws no filled
  container at all; it is how Inbox, Folders, Outputs, Plugins, Apps, and
  the Code empty pages render an empty index, and it is the only icon
  treatment allowed on an empty surface.
- Show the canonical answer in Storybook. Real product examples beat prose
  principles. The `Foundations/Patterns` story shows the right way to
  compose settings, cards, approvals, and empty states so the next builder
  copies a trusted reference instead of inventing a plausible one.

## Pointers

- Humans and commit/PR titles: [`CONTRIBUTING.md`](CONTRIBUTING.md)
- Board: [Tidebreak GitHub Project](https://github.com/orgs/brightwave-inc/projects/5)
- Decisions: [`docs/decisions/`](docs/decisions)
- Parked scope: [`docs/deferred.md`](docs/deferred.md)
- Releases: [`docs/releases.md`](docs/releases.md)
- Cross-provider replay: [`docs/model-providers.md`](docs/model-providers.md)
