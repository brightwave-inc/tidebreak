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
- **Stacking PRs on top of one another is encouraged.** When a slice depends on
  one still in review, branch off that PR's branch and keep going — don't sit
  idle waiting for the base to merge. Retarget the stacked PR to `main` (and
  rebase) once its base lands.
- **Branch off `main`; PR back into `main`.** Never commit straight to `main`.
- **Issues are for deferred or cross-agent work, not for narrating the current
  task.** Open an issue when work is being set aside for later, or when it needs
  to be visible to *other* sessions — a slice another agent might pick up, or a
  claim on contested scope. Work the user is directing interactively in this
  session doesn't need an issue; filing one adds tracking overhead without
  coordinating anyone. If you do pick up substantial parallel-track work, claim
  its issue **before you start editing** — see below.
- **Only commit or push when asked.** Don't merge your own PRs unless the request
  was explicitly to merge; default to opening the PR for review.

## Issue tracking

Work is tracked with plain GitHub **issues** — no project board. Issue state
(open/closed), assignee, and a small set of workflow labels carry everything the
team and separate agent sessions need to see where things stand without reading
commit logs. This matters where sessions can collide or work outlives a session;
it is not a ledger of everything an agent happens to be doing right now.

The workflow labels:

- `in-progress` — claimed; a session is actively working the issue.
- `blocked` — cannot proceed until a dependency or decision lands.
- `deferred` — consciously parked; not on any session's active slate.

The conventions:

- **Claim before you build, not after.** Assign yourself and add the
  `in-progress` label **before the first edit**. Sessions run in parallel and
  cannot see each other's working trees, so the issue is the only place a claim
  is visible. Claiming at the end of the work is worth nothing: the failure it
  prevents is a second session starting the same issue an hour ago.
- **Check for existing work before you start.** Read the issue's labels/assignee
  and `gh pr list` together. Either can be stale on its own — an unclaimed issue
  with an open PR against it is taken. `gh issue list --label in-progress` shows
  every active claim.
- **A closed issue is not proof the work is done.** Check whether the merged PR
  covered the whole scope, and open a follow-up issue for whatever it left.
- **If you find your slice already merged by someone else, don't force yours
  through.** The version that landed first has the floor; rebasing a competing
  design over it reverts reviewed work. Salvage the difference — extra coverage,
  bugs you found — into follow-up issues, and say so when you close yours.
- **One issue per slice.** Reference it from the PR with `Closes #N` so the merge
  auto-closes the issue.
- **Keep labels current.** Stale state misleads the team — drop `in-progress`
  when you park work (and add `deferred` or `blocked` with a comment saying
  where it stands); treat this as part of the change, not an afterthought.
- **Avoid the GitHub GraphQL API — REST covers this workflow entirely.** All
  agent sessions share one `gh` account, and the GraphQL quota (5000 points/hr)
  is routinely exhausted when sessions run in parallel — REST keeps working
  when GraphQL is rate-limited. Everything above is plain `gh` issue/PR
  commands or `gh api <rest-path>`; don't reach for `gh api graphql`, and note
  a few `gh` subcommands (e.g. `gh project *`, which nothing here needs) use
  GraphQL under the hood.

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
  -D warnings`), and the desktop tests. Every cargo invocation passes `--locked`,
  so a lockfile drift fails there too.
- A Rust change also marks the **workspace** scope, which adds the headless
  workspace tests plus the PostgreSQL turn-state lane, unless every changed file
  is one of `openwave-desktop`'s own sources. Nothing in the workspace depends on
  the desktop crate and the headless lane already excludes it, so those lanes
  cannot see such a change. `ui/src/generated/` is not covered by the carve-out
  because its staleness check lives in `openwave-server`.
- Any file under `crates/openwave-desktop/ui/` marks it **UI**, which runs
  `pnpm test` and `pnpm build` as two fixed parallel jobs.
- Branch protection requires the pull-request gate jobs directly. Conditional
  jobs report a successful skip when their scope is false; the always-running
  change detector rejects impossible narrower-scope combinations before those
  jobs consume them. There is no serial aggregate wrapper after the slowest job.
- The heavyweight `sandbox-resident container e2e` lane is scoped separately on
  merges to `main`. It runs when the sandbox agent/protocol, container driver,
  Dockerfile, or dependency/toolchain inputs change. A weekly scheduled run and
  every `workflow_dispatch` exercise it as a backstop. Pull requests drive the
  same host driver against the real sandbox agent over loopback, but only the
  post-merge/scheduled lane proves the Docker packaging and container network
  boundary.

So a scoped skip is not a coverage hole, and duplicating those lanes locally is
wasted time. Run a cheap subset for fast feedback on what you actually touched —
`cargo check -p <crate>`, the one test module you changed, `tsc` — and push.

**The real trap is a PR that got no CI at all.** A conflicting PR runs nothing
but the trivial policy checks, so "no checks failed" reads green while nothing
was verified. Judge by whether every applicable required job is present and
passed, not by the absence of red. `gh pr checks <n>` shows the truth; rebase a
conflicting PR before believing anything about it.

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
