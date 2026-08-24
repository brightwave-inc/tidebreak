# 65. Pull-Request State Is One Store with One Fetcher and One Clock

- Status: Accepted
- Date: 2026-08-24
- Owners: code mode, delivery
- Related: [`0042-user-initiated-pr-merge.md`](0042-user-initiated-pr-merge.md),
  [`0060-triggers-are-durable-rules-on-pull-request-facts.md`](0060-triggers-are-durable-rules-on-pull-request-facts.md),
  [`0062-pull-request-facts-and-attribution.md`](0062-pull-request-facts-and-attribution.md)

## Context

One pull request lives in four representations, each with its own fetcher,
its own cache, and its own clock:

| Representation | Fetcher | Freshness | Read by |
|---|---|---|---|
| `PullRequestDigest` | `gh pr view` ×2 + `gh pr checks` + timeline | 20 s `PrDigestCache` (`gh.rs`) | workspace header, Review tab |
| `code_workspace.pr` column | side effect of a digest read | as old as the last read | workspace cards, `/code/updates` digests |
| `code_pull_request` facts | detector, sweeps, delivery reads | ≤ 61 s, no check state | attributed lists, stacks, triggers |
| Delivery summaries | `gh pr list` / `gh pr view` + REST | 30 s aggregate cache | delivery center, detail sheet, notifications |

Nothing invalidates across them, so surfaces disagree by construction:

- The header pill and the Review pane read the same digest but classify it
  differently. `workflowState` (`prActions.ts`) ranks `blocked` above
  `pending`, and GitHub reports `mergeStateStatus: BLOCKED` whenever *any*
  protection requirement is unmet — including required checks that are still
  running, and the review approval this repository never receives. Every open
  pull request therefore reads "Blocked" for its whole life, while the pane
  beside it shows "Open", nine pending checks, and a reviewer's go-ahead.
- A push never invalidates `PrDigestCache`, so the moments after a push — when
  the reader most wants "checks pending on the new head" — can serve the
  pre-push digest for 20 s.
- The watch sweep skips its state read whenever any session in the workspace
  is running (`watch.rs`), so the pull requests being actively fixed are the
  ones that update least.
- An unwatched, unviewed workspace refreshes its digest never: the column
  only moves when the UI happens to call `GET …/pr`. Workspace cards render
  that column, so a card can be hours old while the fact row under it is
  61 s fresh.
- A merge from the delivery sheet invalidates the delivery cache only; the
  workspace header keeps the pre-merge state until its own TTL expires.

The cost side is as scattered as the freshness side. One uncached digest read
is four to six `gh` subprocesses in three sequential network hops: a
`gh pr view`, then `gh pr checks` alongside a paginated timeline read for
merge-queue state (paid even by repositories with no queue), then a second
`gh pr view` to verify the head did not move between the first two. Three
background sweeps (47 s watch, 53 s trigger, 61 s reconcile) fetch
independently. Almost every read is GraphQL-backed (`gh pr view`, `gh pr
list`, `gh pr checks`) — the exact call pattern that trips GitHub's secondary
rate limits when several loops share a token, which is why our own agent
guidance already bans it for pull-request polling. Nothing sends a
conditional request; every poll pays full price even when nothing changed.

## Decision

**A pull request is one definitive row, written by one fetcher, refreshed on
one schedule, and broadcast on change. Every surface is a projection of that
row, and no surface calls GitHub on its own.** Two goals rank equally: every
surface agrees, and Tidebreak stays a light GitHub citizen — a user running
it all day must never notice it in their rate limit.

1. **The fact row grows a live tier.** `code_pull_request` (decision 62)
   gains the volatile fields the digest carries today: check rollup, review
   decision, mergeability, merge state, auto-merge, queue membership, and an
   `observed_at`. `PullRequestDigest` and `CodeDeliveryPullRequestSummary`
   become projections of the row; the `code_workspace.pr` column becomes a
   write-through copy for wire compatibility. Facts already carry repository
   identity, which also retires the digest's number-collision hazard across
   repositories.
2. **One fetcher, REST, conditional.** `refresh_pull_request(repo, number)`
   is `gh api repos/{o}/{r}/pulls/{n}` plus
   `gh api repos/{o}/{r}/commits/{head_sha}/check-runs`, where the second
   read uses the head SHA the first returned — consistency by construction,
   so the bracketing second `pr view` and its retry loop are deleted. The row
   stores each endpoint's ETag and sends `If-None-Match`; GitHub does not
   count a 304 against the primary rate limit, so the sustained cost of
   polling tracks how often pull requests actually change, not how often
   Tidebreak asks. `gh` keeps owning authentication; Tidebreak still never
   touches a token. The merge-queue timeline read runs only for repositories
   observed to use a queue.
3. **One scheduler, tiered by attention.** A single refresher walks open
   fact rows: *hot* (the workspace the UI reports viewing, an active watch,
   or a head pushed in the last few minutes) every ~15 s; everything else
   rides one conditional `pulls?state=open` list per repository every ~60 s,
   diffed by `updated_at` so only pull requests that moved get a row
   refresh. Check runs do not bump a pull request's `updated_at`, so open
   attributed rows also get a conditional check-runs read on the slow tick;
   settled rows get nothing. Mutations — push, create, mark ready, merge,
   delivery actions — dirty the row and refresh it immediately, which
   closes the push staleness hole. The watch and trigger sweeps stop
   fetching and consume the row; a watch still gates *turn submission* on an
   idle workspace, but no longer skips the state read.
4. **A change broadcasts once.** The store diffs on write; a changed row
   emits workspace digests for every attributed workspace (the existing
   `/code/updates` channel) plus one delivery update, and the delivery page
   and background monitor drop their own poll timers. `GET
   /code/workspaces/{id}/pr` stops calling GitHub in the request path: it
   reads local git plus the row, so opening a workspace stops paying the
   1–3 s digest fetch.
5. **One classifier.** `prWorkflowStatus` becomes the single derivation every
   pill, card, badge, and merge box reads. Two ranking fixes: pending
   required checks outrank the generic `blocked`, and `blocked` with green
   checks and `review_decision: review_required` becomes its own state,
   `needs_approval`, said in those words. The header in the motivating
   screenshot then reads "#2539 · 9 pending" while CI runs and
   "#2539 · Needs approval" once green — the same story the pane tells.
6. **Every GitHub read passes one gate.** The fetcher is the only caller for
   pull-request state, and on-demand reads (comments, reviews, files, job
   logs) go through the same gate rather than around it. The gate holds a
   small global concurrency, spaces requests per host, and treats a
   secondary-rate-limit 403 or a `Retry-After` as an order: park the tier
   for the stated time with backoff, never retry hot. When the app window is
   hidden, every tier slows the way the delivery monitor already does today.
   No view owns a timer, and no view triggers an unconditional fetch.

Deliberately excluded: GitHub webhooks (a desktop app has no public
endpoint; a relay is real infrastructure and conditional polling captures
most of the freshness), parsing reviewer-verdict comments such as "G2G" into
state (deployment-specific; belongs in trigger rules per decision 60 if
anywhere), and a token-holding HTTP client (decision: `gh` owns auth).

## Alternatives Considered

**Tune the TTLs and cross-invalidate the existing caches.** Rejected:
pairwise invalidation across four stores is the complexity that produced
this record, and divergence between independently fetched snapshots remains
structural no matter how the TTLs are set.

**One GraphQL query for everything.** A single round trip could return the
view, checks, and queue state at once. Rejected: GraphQL is the path that
trips secondary rate limits under concurrent loops, it has no conditional
requests, and the repository rule is REST via `gh api` for exactly that
reason.

**Reorder the classifier and stop.** Fixes the screenshot in an afternoon
and none of the staleness. Kept — as slice 1 of this record, not as the
destination.

**Webhooks through a hosted relay.** The only push-true option. Parked, not
rejected: it needs a relay service and delivery to machines behind NAT, and
the gateway direction may eventually provide both. Conditional REST polling
at a 15 s hot tier is within one tick of the same experience at none of the
infrastructure.

## Consequences

- Every surface shows the same snapshot at the same instant; the only skew
  left is paint time. The mismatch class in the motivating screenshot cannot
  recur, because there is nothing separate to disagree with.
- Steady-state GitHub traffic drops from three GraphQL sweeps plus per-view
  fan-outs to a handful of conditional REST reads: two per hot pull request
  per ~15 s, one list per repository per ~60 s, one check-runs read per open
  attributed row per slow tick. Nearly every one returns 304, which GitHub
  does not count against the primary limit — counted traffic scales with
  change, not with polling. The 20 s `PrDigestCache`, the double-`pr view`
  head dance, and the unconditional timeline pagination are deleted rather
  than tuned.
- Freshness stops depending on the reader: a card in the sidebar, the header
  pill, and the delivery row age together, bounded by the row's tier rather
  than by which endpoint the component happens to call.
- The work stages as independent slices: (1) classifier ranking +
  `needs_approval` + the push/action invalidation holes, no schema; (2) the
  live tier on facts with write-through and change broadcast; (3) sweep
  consolidation onto the store; (4) the conditional REST fetcher; (5) UI
  surfaces drop their poll timers. Slice 1 alone fixes the screenshot; each
  later slice removes a whole mechanism rather than adding one.

Revisit when a second forge (GitLab) arrives — the row and fetcher are the
seam it would plug into — or if the hot tier's conditional polling still
reads as laggy next to a webhook-fed tool.

## Validation

- The screenshot as a test: a digest with `merge_state_status: blocked`,
  `review_decision: review_required`, and nine pending checks classifies as
  `pending`; the same digest with green checks classifies as
  `needs_approval`; every surface renders those words.
- A push dirties the row: the snapshot read immediately after a push carries
  the new head SHA, never the pre-push digest.
- A 304 keeps the row: the fetcher with a stored ETag leaves fields
  untouched, advances `observed_at`, and schedules the next tick.
- A secondary-rate-limit 403 parks the tier for the `Retry-After` window and
  the next read goes out after it, not before — asserted against a scripted
  `gh` that answers 403 once.
- A merge from the delivery sheet reaches the workspace header in one
  broadcast, with no residual cache window.
