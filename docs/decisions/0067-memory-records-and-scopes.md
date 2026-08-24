# 67. Memory Records and Scopes

- Status: Proposed
- Date: 2026-08-24
- Owners: memory
- Related: decisions 19, 31, 48, 59, 61; [docs/code-mode.md](../code-mode.md)

## Context

Nothing durable and curated survives a session. Chats have transcripts and
semantic checkpoints (decision 19); code sessions have journals and per-turn
recaps. All of it is episodic: a compression of one conversation, owned by
that conversation. A preference the user states in one chat is relearned in
the next. A fact about a repository proven in one workspace is rediscovered
in the next worktree. The knowledge exists in the product's own records; no
structure carries it forward.

Whatever carries it forward inherits standing constraints. It must be
engine-neutral (decisions 30 and 31): external harnesses consume it too, so
the format cannot assume the internal loop or a coding agent. It must be
owner-scoped and code-shaped (decision 48), or convergence step 5 has to
translate it. Schema changes are appended migrations (decision 61). And it
is user data: the user must be able to see, edit, and truly delete every
piece of it.

The failure modes are well documented wherever long-lived agent memory has
shipped. Stores grow without bound until whatever selects from them starts
missing. Model-written entries acquire authority nobody granted: one wrong
"fact" silently degrades every later session. Derived summaries hide what
the system believes from the person it believes it about. Deleting a source
does not delete what was already distilled from it. Stale entries mislead
with the confidence of fresh ones. And memory is a persistence vector:
content planted in a store today shapes behavior weeks later, across
sessions — which is what separates memory poisoning from ordinary prompt
injection and makes provenance and review load-bearing rather than nice.

## Decision

**A memory record is a markdown document with a typed envelope.** The body
is plain markdown, capped near 2 KiB. The envelope carries: `id`; `scope`;
`kind` (`fact | preference | lesson | reference`); `status`; `title` — one
line, written as a retrieval hook that says when the record matters;
provenance (`author`: user, model, or import; origin session, turn, and
workspace ids where present; `evidence`: references to the specific journal
events or messages that justify the record); optional links to other
records; optional `expires`; created and updated timestamps. Markdown plus
this envelope is the interchange contract every storage backend must speak;
indexes of any kind stay backend-internal, mirroring the flatten-on-switch
rule in [docs/model-providers.md](../model-providers.md).

**Evidence is enforced at the storage layer.** A model-authored record
without resolvable evidence references is rejected — not stored with the
references stripped, and not stored evidence-less. A memory the system
cannot justify is not expressible. User-authored records carry authorship
instead.

**Status is a lifecycle: `tracking → proposed → active → archived`, plus
`rejected`.** Only `active` records carry authority. Model-authored records
enter as `proposed` and require user action to activate; a per-scope
auto-commit opt-in exists and defaults to off. Weak signals do not reach
review at all: a single observation of a pattern becomes a `tracking`
record — a durable hypothesis carrying an observation count and its
evidence — and graduates to `proposed` only after the pattern repeats
across distinct sessions. Hypotheses are visible in the manager, never
injected, and expire mechanically when not re-observed. Dismissed
proposals persist as `rejected` for a horizon so capture can see them and
not re-propose. A record superseded by a merge is archived with a
`superseded_by` pointer; every mutation appends a revision, so what the
system believes, on what evidence, and since when is always answerable.

**Scopes are `personal` and `repo`, and the enum is non-exhaustive.**
Personal records follow the owner across every surface. Repo records bind
to a registered repository — the durable identity — never to a workspace,
which reclaim tiers (decision 59) can erase. Chats use the personal scope;
when decision 48 reaches step 5, a conversation bound to a repo-backed
workspace picks up that repo's scope with no new structure. Wider scopes
are future enum variants, not a redesign.

**The store is bounded, and overflow coaches instead of evicting.** Each
scope has an active-record cap and a digest byte cap. A write that would
breach the cap fails with an error that names the outs — consolidate
overlapping records, archive something no longer true, or update an
existing record instead of adding — and nothing is ever silently evicted
or truncated.

**The digest is a derived render, never a stored artifact.** What injects
into context is a deterministic render of the active records: title lines
with absolute dates, grouped by kind, ordered by recency of update,
framed as dated point-in-time claims rather than current truth. Because
the digest is recomputed from records, deleting a record deletes it
everywhere; there is no distilled copy that survives its source.

Deliberately excluded: any hidden derived profile the user cannot read and
edit; ingestion of external documents or knowledge sources (records derive
from the user's own work in this product); and mirroring an engine's own
instruction or memory files into records — files in a worktree are
repository-owned changes, reviewed as code.

## Alternatives Considered

**An unbounded store with retrieval ranking.** Rejected. Once the store
outgrows its context budget, a selection step exists, and every selection
step can miss — important-but-dissimilar entries vanish exactly when they
matter, and stale entries accumulate instead of being confronted. The
bounded active set makes recall exhaustive (every record's title is always
present) and moves precision to write time, where thresholds and review
are cheap.

**A rolling profile summary.** One model-maintained text rewritten in
place is compact but unauditable: entries cannot be individually deleted
or dated, a bad rewrite silently drops or inverts beliefs with no trail,
and the user cannot see what changed. Every failure mode in the Context
section lands on this shape.

**Workspace scope.** Rejected: workspaces are deliberately ephemeral.
Knowledge keyed to them fragments across worktrees and dies with reclaim.

**Do nothing — checkpoints and recaps suffice.** Rejected. Both are
episodic and conversation-owned. Neither is scoped, curated, reviewable,
or injectable across sessions, and decision 19 deliberately scopes
checkpoints to a conversation's own cache.

## Consequences

New tables arrive as appended migrations (decision 61). The review queue
becomes mandatory product surface: a memory system whose writes wait for
review without a place to review them is not shipped. Caps mean curation
pressure is a user-visible experience, by design. Evidence-at-storage
makes audit possible and deletion total, and costs a resolution step on
every model write. Chat-side structure beyond the personal scope waits for
decision 48 step 5; demand for a project-like scope before then would
force this record open again.

## Validation

Deleting a record removes it from the next digest render and from search
results, with nothing derived retaining it. A plausible wrong
implementation stores the rendered digest as its own artifact and keeps
serving deleted content; the deletion test must force a render and assert
absence. A model-authored write with unresolvable evidence is rejected
loudly. At the cap, a create fails with the coaching error and the store
is unchanged. `tracking` records never appear in any digest render. Two
renders of an identical store are byte-identical.
