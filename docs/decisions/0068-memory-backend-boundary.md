# 68. Memory: One Record Vocabulary Behind a Backend Boundary

- Status: Proposed
- Date: 2026-08-24
- Owners: memory
- Related: decisions 19, 21, 31, 33, 35, 48, 50, 53, 60, 61, 67;
  [docs/model-providers.md](../model-providers.md)

## Context

Decision 67 defines what a memory record is. This record defines where
records live, how they reach a session, and how they get written — under
constraints this repository has already committed to. Engines are external
and varied, and a session's engine may not be a local child process
(decision 31; the remote-execution entry in
[docs/deferred.md](../deferred.md)). Some installs run with zero model
configuration, because harnesses bring their own authentication. Prompt
caching economics punish any per-turn churn in the composed prompt
(decision 19 exists because of them). The session journal has a single
writer (decision 35). Background work is a durable sweep reading rows, not
a prompt loop (decisions 50 and 60), and anything an agent receives that
the user did not type names its own origin (decision 60). Users also
already run their own memory tooling as MCP servers; the product should
compose with that, not fight it.

## Decision

**One trait, exhaustive capabilities, in-tree backends.** Storage sits
behind a `MemoryBackend` trait in `tidebreak-core`, with the record types
beside the domain model. Operations: verbatim put, ingest (extraction
intent, optional), get, list, update, set-status, delete, search,
assemble-context (the digest render), and revision history. Each backend
states a `MemoryCaps` value for every capability flag — extraction,
lexical search, semantic search, consolidation, context assembly, revision
history, verified delete, asynchronous writes, agent-editable surfaces —
as `Supported`, `Unsupported`, or `Unknown`, constructed exhaustively with
no `Default`, exactly as decision 31 requires of harness adapters. An
unsupported capability degrades visibly. Backends are in-tree; a
third-party store lands as a PR, not a plugin. Write receipts are honest
about the layer they guarantee: `Committed` or `Pending`, and the default
backend commits row, digest render, and revision in one transaction.

**The default backend is rows plus lexical search.** Records live in
owner-scoped database rows holding the markdown documents verbatim.
Search scores full bodies in-process and returns the record id, title,
date, and matching line — an injectable bundle in one call. No embeddings,
no vector store, no graph, and no model calls on the read path. Semantic
retrieval is a capability a future or third-party backend declares, and it
must ship with a readiness signal that distinguishes "index not ready"
from "no results".

**Third parties plug in at two depths, and never as mirrors.** Tool depth:
any memory MCP server mounts through the existing MCP runtime today, zero
code. Backend depth: the trait. What is rejected in both cases is a live
synchronization with an engine's native memory files — semantics we
cannot honor (no proposed state, no provenance, no review) drifting in
both directions. The session journal is the neutral observation point;
one-time import through the export format covers migration.

**Injection is a pinned snapshot.** Work mode: the digest renders into one
section of the composed prompt, pinned per conversation — rendered at the
first turn and reused verbatim until a boundary that already rebuilds the
prefix (a new conversation, a compaction checkpoint per decision 19, a
model switch, an explicit refresh). A record saved mid-conversation
reaches the store immediately and the prompt at the next boundary; its
content is already in the transcript that produced it. Code mode: the
digest and record bodies materialize as read-only markdown files in the
session's private read root — never inside the worktree, so the repository
cannot index them — and the path is named once in the first turn's engine
text. Engines with a config-injected MCP surface additionally get a memory
verb (propose, search, read) over the loopback bridge that decision 33
established for approvals, gated per adapter by an honest capability flag.
Every injected digest names its origin and frames records as dated
point-in-time claims the current conversation overrides.

**Capture is post-turn, model-supplied by this product, and bounded.**
After a turn's journal write commits, one structured-output derivation
runs on the utility model role: the same machinery that produces titles
and recaps, one-shot cache mode, claim-gated per session, skipped for
turns with no substantive activity. It reads the turn's journaled material
plus the current digest, tracked hypotheses, and recently rejected titles,
and yields nothing, an update to an existing record, a proposal, or a
hypothesis observation — under decision 67's thresholds. No engine ever
runs memory derivation: the memory plane works identically for every
harness and for installs with no harness at all. Where no utility model
resolves, capture and maintenance are off and the settings surface says
so; injection and explicit writes, which need no model, keep working.

**Maintenance is a durable sweep.** Consolidation and expiry follow the
decision 50/60 shape: work list read from rows each tick, a per-scope
fingerprint so a standing condition fires once, one bounded utility-model
step per tick, running only while the owner has no active turn. Merge
suggestions are proposals citing their source records; approval activates
the merge and archives the sources with `superseded_by` in one
transaction. Dismissal parks the scope until its record set changes.
Passes are rate-bounded and the last run is visible.

Deliberately excluded: multi-call write-time adjudication pipelines (an
extraction call plus a per-candidate judgment call on every write — the
cost and fragility live on the hot path and the failure modes are silent);
a long-lived curator agent whose own conversation is the store of record
(unbounded hidden state, with invariants enforced only by prompt rules);
and auto-committed model writes, per decision 67.

## Alternatives Considered

**Adopt an external memory service as the default store.** Rejected. The
product surface — capture, review, injection, materialization, the
manager, scoping, governance — exists in every variant and is the bulk of
the work; a service default adds a runtime dependency a local-first
desktop app cannot carry, cannot express the proposed/active lifecycle or
storage-enforced evidence, and would make one vendor's schema the neutral
contract every other backend must translate to. External systems remain
first-class through MCP today and the trait when demand shows.

**Re-render the digest every turn.** Rejected: every accepted record would
invalidate the conversation's prompt cache from the system prompt down.
The pinned snapshot costs a bounded staleness window that the transcript
itself covers.

**Let the working engine write memories mid-turn.** Rejected: it burns
foreground latency and context, couples capture to engine cooperation the
capability flags say we do not have everywhere, and judges signal at the
moment of maximum recency bias. Post-turn distance plus repetition
thresholds is the deliberate posture.

**Do nothing and rely on user-mounted MCP memory tools.** Rejected as the
default: nothing injects, so recall depends on the model deciding to
search; nothing is reviewed, so writes accrue authority ungated; and
nothing spans engines that lack MCP support. It remains fully supported as
an option.

## Consequences

Work-mode turn completion gains its first post-completion hook, beside the
existing front-of-turn derivations. `HarnessCaps` grows a flag for
config-injected MCP tools, which every adapter must answer; engines
without it show a stated limitation on the memory verb. The journal stays
single-writer — capture reads events and writes memory rows, never journal
events. Model cost is one small structured call per qualifying turn plus
rate-bounded maintenance, both on the cheapest configured route and both
visible. Zero-model installs get a reduced but honest feature. Revisit if
a provider ships a server-side memory primitive worth delegating to, or if
a real third-party backend cannot be expressed in the capability
vocabulary.

## Validation

The cache invariant is the test that matters most: activate, update, and
delete records mid-conversation, then assert the next turn's composed
prompt fingerprint is byte-identical to the previous turn's. A plausible
wrong implementation re-renders the digest from the live store on every
turn — it passes every functional test and silently pays a full cache
rewrite on each memory change; only the fingerprint assertion fails it.
`MemoryCaps` construction is compile-exhaustive, so a new flag breaks
every backend until answered. With no resolvable utility model, the
settings surface reports capture as not configured rather than silently
idle. Materialized memory files never resolve to a path inside the
worktree. A turn with no substantive activity produces no derivation
call.
