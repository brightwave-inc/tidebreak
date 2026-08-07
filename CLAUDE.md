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
- **Branch off `main`; PR back into `main`.** Never commit straight to `main`.
- **Issues are for active or cross-agent work, not for narrating the current
  task or keeping a someday backlog.** Use an issue when work needs a concrete
  owner, dependency, or reviewable delivery. Keep deliberately parked product
  scope in [`docs/deferred.md`](docs/deferred.md), which is the canonical
  account of what OpenWave does not yet implement and why. Work the user is
  directing interactively in this session doesn't need an issue; filing one
  adds tracking overhead without coordinating anyone. If you do pick up
  substantial parallel-track work, claim its issue **before you start editing**
  — see below.
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
- **If you find your work already merged by someone else, don't force yours
  through.** The version that landed first has the floor; rebasing a competing
  design over it reverts reviewed work. Salvage the difference — extra coverage,
  bugs you found — into follow-up issues, and say so when you close yours.
- **Reference the issue from the PR** with `Closes #N` so the merge auto-closes
  it.
- **Keep labels current.** Stale state misleads the team — drop `in-progress`
  when you park work. If the work is blocked by a concrete dependency, add
  `blocked` with a comment saying what is needed. If it is deliberately future
  product scope rather than an active, actionable task, close the issue and
  update [`docs/deferred.md`](docs/deferred.md) instead; do not add a
  `deferred` label.
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
- **Cross-provider replay stays flatten-on-switch** — see
  [`docs/model-providers.md`](docs/model-providers.md). Native artifacts are
  origin-gated; foreign routes get cleartext (or nothing), never a new
  pairwise translator. New provider-coupled features need a degradation story
  in the same change.
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

During active greenfield development, pull requests use a deliberately fast
default gate. Full validation remains automatic after merge and on scheduled or
manual runs. Add `full-ci` when a change warrants the heavyweight
platform-neutral lanes before merge, and add `windows-ci` independently when it
warrants the native Windows lane. Re-running the full workspace suite before
every push buys little and costs minutes on each iteration. Run a cheap relevant
subset locally, push early, and let the configured lanes work while you keep
going.

The change-scope gate in [`.github/workflows/ci.yml`](.github/workflows/ci.yml)
is the whole rule, and it is coarse on purpose:

- Any changed file outside `*.md`, `docs/`, `assets/`, `.githooks/`, `LICENSE`,
  `NOTICE`, and `crates/openwave-desktop/ui/` marks the change **Rust** —
  including `Cargo.toml` and `Cargo.lock`. That always runs rustfmt. With
  `full-ci`, and automatically outside pull requests, it also runs clippy
  (`--all-targets -D warnings`) and the desktop tests. Every Cargo invocation
  passes `--locked`, so a lockfile drift fails there too.
- A Rust change also marks the **workspace** scope, which adds the headless
  workspace tests plus the PostgreSQL turn-state lane, unless every changed file
  is one of `openwave-desktop`'s own sources. Nothing in the workspace depends on
  the desktop crate and the headless lane already excludes it, so those lanes
  cannot see such a change. `ui/src/generated/` is not covered by the carve-out
  because its staleness check lives in `openwave-server`.
- Any file under `crates/openwave-desktop/ui/` marks it **UI**, which runs
  `pnpm test` and `pnpm build` together during full CI.
- Branch protection requires the pull-request jobs directly. Heavy jobs report
  a successful skip on ordinary pull requests and become active when the
  `full-ci` label is added. The always-running change detector rejects
  impossible narrower-scope combinations before conditional jobs consume them.
  There is no serial aggregate wrapper after the slowest job.
- The native Windows lane checks the Windows-gated crates plus the host-broker,
  desktop SQLite profile, and Credential Manager contracts. It runs
  automatically for Rust-scoped pushes to `main`, weekly schedules, and manual
  dispatches; a pull request runs it only with `windows-ci`. Use that label for
  Windows-gated code, host-broker or sidecar packaging, desktop persistence or
  keychain work, Windows release inputs, and native dependency or toolchain
  changes. `full-ci` does not imply `windows-ci`. Superseded Windows jobs on
  linear `main` are cancelled because the newer Rust-scoped state covers the
  older one; pull-request, scheduled, and manual runs remain isolated.
- The heavyweight `sandbox-resident container e2e` lane is scoped separately on
  merges to `main`. It runs when the sandbox agent/protocol, container driver,
  Dockerfile, or dependency/toolchain inputs change. A weekly scheduled run and
  every `workflow_dispatch` exercise it as a backstop. Pull requests drive the
  same host driver against the real sandbox agent over loopback, but only the
  post-merge/scheduled lane proves the Docker packaging and container network
  boundary.

An ordinary pull request's heavy-job skips are the intended fast path, backed by
full validation on `main`. For risky or boundary-crossing changes, add
`full-ci` and wait for the applicable platform-neutral lanes; add `windows-ci`
too when the Windows boundary is relevant. Run a cheap subset for fast local
feedback on what you actually touched — `cargo check -p <crate>`, the one test
module you changed, `tsc` — and push.

**The real trap is confusing the fast gate with full validation.** When a PR has
`full-ci`, judge by whether every applicable platform-neutral job is present and
passed, not by the absence of red. When it also has `windows-ci`, wait for the
native Windows lane as well. A conflicting PR runs nothing but trivial policy
checks; rebase it before believing its status. `gh pr checks <n>` shows the
truth.

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
