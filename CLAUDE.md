# Working in OpenWave

Guidance for Claude (and other coding agents) working in this repository. Humans
should read [`CONTRIBUTING.md`](CONTRIBUTING.md) first — this file layers the
day-to-day standards an agent needs on top of it, and does not repeat what's
already there.

OpenWave is a Cargo workspace of Rust crates plus a Tauri desktop app whose UI is
React/TypeScript under `crates/openwave-desktop/ui`.

## Workflow

- **Do not use the `bw` CLI in OpenWave.** It is tooling for the separate Alpha
  repository, not this one; use this repository's documented Cargo, pnpm, and
  GitHub commands instead.
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

## Tests earn their place or come out

Build time is a first-class cost here — the workspace is large and the Rust lanes
dominate CI. A test that would never change what we do is not free; it is paid
for on every build, by everyone, forever. Write fewer, better tests.

The bar for adding one: **would this failing tell us something we'd act on?**

Worth writing, and worth defending in review:

- **Contracts that cross a boundary** — wire types, persisted shapes, migration
  compatibility. Breaking these silently is expensive to discover.
- **Decisions that are easy to reverse by accident** — the model registry's
  honesty invariants, the guard that no model advertises image input before the
  path carries it, the check that the default model is curated and current.
- **Reproductions of bugs we actually hit.** These are the highest-value tests
  in the repo.
- **Behavior, driven end to end.** One test that runs a real turn and reads the
  journal beats ten that assert on intermediate structs.

Not worth writing, and fair game to delete on sight:

- Tests that assert the code *exists* — constructing a struct and reading its
  fields back, or checking a constant equals itself.
- Duplicate coverage: several tests walking one path with cosmetically different
  inputs. Keep the one that best localizes a regression.
- Assertions pinned to internals that break on every refactor without ever
  catching a defect. These tax exactly the changes we want to be cheap.
- Over-specified assertions — matching a whole serialized payload when the test
  is about one field. They fail for unrelated reasons and train people to update
  expectations without reading them.
- Setup-heavy tests whose assertion is trivial next to the scaffolding.

When you delete tests, justify each one in the PR body in a line. "Removed 14
tests" is not reviewable; "removed 14 that re-asserted serde round-tripping
already covered by the wire-type fixtures" is.

Coverage percentage is not a goal and is not tracked. Confidence is.

## Let CI do the heavy verification

CI is the gate, and for anything it runs at all it runs *more* than you can
locally. Re-running the full workspace suite before every push buys nothing and
costs minutes on each iteration. Push early and let the lanes work while you
keep going.

The change-scope gate in [`.github/workflows/ci.yml`](.github/workflows/ci.yml)
is the whole rule, and it is coarse on purpose:

- Any changed file outside `*.md`, `docs/`, `assets/`, `LICENSE`, `NOTICE`, and
  `crates/openwave-desktop/ui/` marks the change **Rust** — including
  `Cargo.toml` and `Cargo.lock`. That runs rustfmt, clippy (`--all-targets
  -D warnings`), the rich document-parser contracts, and the desktop tests.
  Every cargo invocation passes `--locked`, so a lockfile drift fails there too.
- A Rust change also marks the **workspace** scope, which adds the headless
  workspace tests with the server's rich parser adapters disabled, plus the
  PostgreSQL turn-state lane, unless every changed file is one of
  `openwave-desktop`'s own sources. Parser-specific tests run in the already-rich
  desktop lane instead of linking PDF/Office/image/spreadsheet support into every
  headless test binary. Nothing in the workspace depends on the desktop crate and
  the headless lane already excludes it, so those lanes cannot see such a change.
  Its `Cargo.toml` is not covered by the carve-out — it forwards features into
  `openwave-server` — and neither is
  `ui/src/generated/`, whose staleness check lives in `openwave-server`.
- Any file under `crates/openwave-desktop/ui/` marks it **UI**, which runs
  `pnpm test` and `pnpm build` as parallel matrix jobs.
- The aggregate `fmt · clippy · build · test` check asserts each lane either
  succeeded or was legitimately skipped, and branch protection requires it. A
  lane is skipped only when the change could not have affected it.
- The two heavyweight capability lanes are scoped separately on merges to
  `main`. `durable vector store` runs when retrieval, its server/CLI/desktop
  feature wiring, or dependency/toolchain inputs change. `sandbox-resident
  container e2e` runs when the sandbox agent/protocol, container driver,
  Dockerfile, or dependency/toolchain inputs change. A weekly scheduled run and
  every `workflow_dispatch` exercise both as a backstop.
- **One exception, and it is a real coverage gap:** neither heavyweight lane
  runs on pull requests. If you touch `openwave-retrieval`, the `vec-lance`
  feature, or the release guard in `openwave-server`'s build script, run
  `cargo test -p openwave-retrieval --features vec-lance` locally — a PR going
  green says nothing about LanceDB. Tracked in #760. The container path has
  lower residual risk because PRs drive the same host driver against the real
  sandbox agent over loopback, but only the post-merge/scheduled lane proves the
  Docker packaging and container network boundary.

So a scoped skip is not a coverage hole, and duplicating those lanes locally is
wasted time. Run a cheap subset for fast feedback on what you actually touched —
`cargo check -p <crate>`, the one test module you changed, `tsc` — and push.

**The real trap is a PR that got no CI at all.** A conflicting PR runs nothing
but the trivial policy checks, so "no checks failed" reads green while nothing
was verified. Judge by whether the required aggregate check ran and passed, not
by the absence of red. `gh pr checks <n>` shows the truth; rebase a conflicting
PR before believing anything about it.

Enable auto-merge (`gh pr merge <n> --squash --auto --delete-branch`) so a PR
lands the moment its lanes go green instead of waiting for you to come back.

Driving the running app to confirm behavior is not expected — say what you
could not verify and hand it over.

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
