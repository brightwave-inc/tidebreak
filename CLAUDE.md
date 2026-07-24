# Working in OpenWave

Guidance for Claude (and other coding agents) working in this repository. Humans
should read [`CONTRIBUTING.md`](CONTRIBUTING.md) first — this file layers the
day-to-day standards an agent needs on top of it, and does not repeat what's
already there.

OpenWave is a Cargo workspace of Rust crates plus a Tauri desktop app whose UI is
React/TypeScript under `crates/openwave-desktop/ui`.

## Workflow

- **Land work as small, focused PRs — one logical change each.** Don't accumulate
  a large multi-slice diff on a working branch. Ship a slice, open the PR, move
  on. Small and reviewable beats big and sprawling.
- **Branch off `main`; PR back into `main`.** Never commit straight to `main`.
- **Track substantial work on the board first.** For anything beyond a small fix,
  open (or pick up) an issue, put it on the project board, and move it to
  *In progress* **before you start editing** — see below. Several agent sessions
  run in parallel against this repo and the board is how they avoid each other;
  an issue claimed after the work is done has already failed at its job.
- **Only commit or push when asked.** Don't merge your own PRs unless the request
  was explicitly to merge; default to opening the PR for review.

## Project board

Work is tracked on a repo-scoped GitHub **Project** so the team and separate
agent sessions can see where things stand without reading commit logs.

- **Claim before you build, not after.** Assign yourself and move the issue to
  *In progress* **before the first edit**. Sessions run in parallel and cannot
  see each other's working trees, so the board is the only place a claim is
  visible. Claiming at the end of the work is worth nothing: the failure it
  prevents is a second session starting the same issue an hour ago.
- **Check for existing work before you start.** Read the issue's board status and
  `gh pr list` together. Either can be stale on its own — an unclaimed issue with
  an open PR against it is taken.
- **A closed issue is not proof the work is done.** Check whether the merged PR
  covered the whole scope, and open a follow-up issue for whatever it left.
- **If you find your slice already merged by someone else, don't force yours
  through.** The version that landed first has the floor; rebasing a competing
  design over it reverts reviewed work. Salvage the difference — extra coverage,
  bugs you found — into follow-up issues, and say so when you close yours.
- **One issue per slice.** Reference it from the PR with `Closes #N` so the merge
  auto-closes the issue and advances its board status.
- **Keep status current.** A stale board misleads the team — treat updating it as
  part of the change, not an afterthought.
- Managing the board (not issues) needs the `project` OAuth scope on the `gh`
  token; `gh auth refresh -s project` grants it.

## Dependencies

- **Pin every direct dependency to an exact version.** Cargo deps use `=x.y.z`
  (in the root `[workspace.dependencies]` or the crate's `Cargo.toml`); the
  desktop UI uses exact versions in `package.json`, enforced by
  `ui/.npmrc` (`save-exact=true`).
- **A pin must match what the lockfile already resolves** — adding pins should
  leave `Cargo.lock` / `pnpm-lock.yaml` versions unchanged. Verify with
  `cargo check --workspace --locked` (no lock diff) before committing.
- **Bump versions deliberately.** Dependabot edits the pin in its own PR; that's
  the intended upgrade path, not `cargo update` / `pnpm update` drift.

## Verify locally — CI does not cover every change

CI has a change-scope gate that **skips** the heavy Rust build/test/clippy lanes
for many PRs (docs-only, dependency-only, and some server/desktop changes). A
green "mergeable / no checks failed" is **not** proof the code compiles. Before
opening a PR that touches Rust, run what CI would have run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace
cargo check --workspace --locked
```

For the desktop UI, run `tsc`, the vitest suite, and a production build under
`crates/openwave-desktop/ui`.

## Commits and PRs

- Follow the semantic PR-title policy in [`CONTRIBUTING.md`](CONTRIBUTING.md).
  The title becomes the squash commit and controls the next version; use `!`
  only for an intentional breaking change. Published `vX.Y.Z` tags, not
  committed Cargo/Tauri placeholders, are the desktop product's version source.
- Commit and PR text should read as ordinary engineering writing: describe what
  changed and why, not the tooling that produced it.
- This is a public repository. Do not reference internal or private design
  documents in code, comments, commits, or PRs.
- Never commit secrets — see [`CONTRIBUTING.md`](CONTRIBUTING.md).

Release maintainers should follow [`docs/releases.md`](docs/releases.md),
especially the compatibility and desktop-schema checklist before `1.0.0`.
