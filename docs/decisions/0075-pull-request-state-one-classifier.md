# 75. Pull-request state is one classifier with GitHub's colors

- Status: Accepted
- Date: 2026-08-26
- Owners: code mode, delivery
- Related: [`0066-pull-request-state-is-one-store.md`](0066-pull-request-state-is-one-store.md),
  [`0077-pull-request-facts-and-attribution.md`](0077-pull-request-facts-and-attribution.md)

## Context

Decision 66 unified where pull-request state *lives*. Where it becomes an
answer was still scattered: five UI vocabularies (`PrChipTone`,
`PullRequestLifecycle`, the 14-state `PrWorkflowState`,
`WorkspaceWorkflowTone`, and per-component `Badge` variant maps in `PrCard`
and `CodeInspector`) re-derived "what state is this pull request?" from the
same wire fields, with three different precedence ladders and three different
color answers for `merged`:

| Surface | Merged rendered as |
|---|---|
| Workspace card, sidebar, delivery detail badge, delivery row icon | purple (`merged` tone) |
| `PrCard` chip, Review-tab header badge | blue (`info` variant) |
| Workspace header workflow control | green (`ready` tone — the type had no `merged` member) |

`closed` was red everywhere except the delivery row, where the same file's
lifecycle table said `critical` and its row-status function said `neutral`.
A draft pull request rendered a green `open` chip with the raw lowercase
state token as its label in `PrCard`. Merge-queue membership overwrote the
lifecycle word ("Queued") on every compact surface while `pullRequestListStatus`
called the same state "In merge queue", and review requirements were `outline`
in one badge and `warning` in another.

Meanwhile the host grew a real state model we were not reading: GitHub
stacked pull requests entered public preview on 2026-07-30 with a REST
surface (`GET /repos/{owner}/{repo}/stacks`), and our stack edges were
branch-name inference only.

## Decision

One module, `src/code/prState.ts`, is the only place a pull request's state
becomes a label, a tone, a group, or a chip set. Both wire shapes — the
workspace digest and the delivery summary — satisfy its input type, so no
surface needs an adapter or its own copy of the ladder.

- **Lifecycle** (`draft | open | merged | closed`) paints GitHub's own colors
  everywhere: green open, gray draft, purple merged, red closed. The merge
  evidence (`merged_at` / `merged`) outranks the state token. Nothing
  downstream may recolor a lifecycle.
- **Gate** is one ladder: conflict > changes_requested > failing checks >
  queue membership > behind > pending checks > needs approval > blocked >
  auto-merge > ready / checking. A queue entry does not erase a failure the
  reader still has to act on; pending checks outrank the generic block
  (decision 66's rule). Terminal lifecycles gate as themselves.
- **Queue membership is its own chip** in info blue, beside the lifecycle
  word, never a replacement for it. Auto-merge the same. Amber stays
  reserved for states the reader can act on.
- **Review requirements render neutral**, matching GitHub's branch-rule
  fact, not a warning.
- **The merge box lists every blocker**, in GitHub's words; the headline
  picks one.
- The server's `pull_request_attention` ladder is the same order
  (conflicts first), so filters and the UI cannot disagree.

**Stacks come from the host.** The delivery read fetches
`GET /repos/{owner}/{repo}/stacks` per repository (both readers), annotates
summaries with `stack_number`, `stack_size`, and a host-derived
`stack_parent_number`; branch inference remains the fallback for hosts
without the feature, and a failed stacks read degrades silently. The detail
sheet carries the full chain and a stack map; its merge offer is
**Merge stack**, which chains per-layer merges bottom to top with a fresh
head read per hop, skips merged layers, and stops below the first draft —
GitHub's "land everything under the latest ready pull request".

Merge-queue *membership* stays on the `mergeable_state == "queued"` signal:
`gh pr list --json` has no queue field (open cli/cli#12771), the app's gh
runner refuses GraphQL argv by design and test, and REST has no merge-queue
endpoint. Queue entry state and position are out of reach until that
changes.

## Alternatives Considered

- **Fix the color table only** (the minimal patch). Leaves five
  vocabularies and three ladders in place; the next surface re-derives its
  own answer and the fragmentation returns.
- **Derive the gate server-side and put it on the digest wire.** Rejected
  for this change: the workspace digest has eight construction sites and no
  stack knowledge; the UI already receives every field the ladder needs, and
  the trigger system keeps its own server-side classifier. One client
  classifier with the server's order pinned by test is the same guarantee
  for less wire.
- **Queue membership first in the ladder** (what the delivery row did).
  Rejected: a queued pull request whose checks failed on the merge ref is
  about to be evicted; "In merge queue" hides the actionable truth.
- **Fetch `mergeQueueEntry` via `gh api graphql`.** Rejected: the gh runner
  refuses GraphQL by design (a tested security posture), and the shared
  GraphQL quota is a stated constraint in this repository.

## Consequences

- `PrCard`, `CodeInspector`, `WorkspaceCard`, `WorkspacePrList`,
  `WorkspaceWorkflowControl`, the delivery rows, and the detail sheet all
  render from `prState.ts`; the `prStateVariant` copies, `PrChipTone`, the
  duplicate check counters, and both client precedence ladders are deleted.
- `WorkspaceWorkflowTone` gained `merged`; a merged pull request is purple
  in the workspace header too.
- Copy changes shipped: "Queued" → "In merge queue", "Auto-merge armed" →
  "Auto-merge on", "Review required" is neutral, and the merge-box
  sentences are GitHub's ("Resolve the conflicts with the base branch
  first.").
- The hand-regenerated `generated/wire.ts` must match `ts-rs` byte for
  byte; CI's freshness check remains the guarantee. Field order follows
  Rust declaration order.
- What would reopen this: `gh pr list --json` gaining a merge-queue field,
  or the stacks REST surface gaining queue or per-layer merge-state
  information. Either would move queue detection off `mergeable_state` and
  into first-class fields.
